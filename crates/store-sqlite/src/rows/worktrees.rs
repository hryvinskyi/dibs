//! The holds of the worktree pools of members' machines, one row per held worktree.

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{StoreResult, WorktreeRows};
use dibs_domain::ids::{ItemId, MemberId, ProjectId, SessionId};
use dibs_domain::report::MachineId;
use dibs_domain::session::HostName;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::{
    AgentKey, Holder, PoolId, PoolKey, WorktreeHold, WorktreeName, WorktreePool,
};

use crate::SqliteTx;
use crate::codec::{Fields, int, nanos};
use crate::error::corrupt;

const TABLE: &str = "worktree_holds";

/// The columns of a hold, the pool's key first.
macro_rules! columns {
    () => {
        "project_id, member_id, machine, pool, worktree, session_id, agent, host, purpose, \
         item_id, taken_at, last_seen"
    };
}

fn key(f: &Fields<'_>) -> StoreResult<PoolKey> {
    Ok(PoolKey {
        member: MemberId(f.number("member_id")?),
        machine: f.parse("machine", MachineId::parse)?,
        pool: f.parse("pool", PoolId::parse)?,
    })
}

fn hold(f: &Fields<'_>) -> StoreResult<WorktreeHold> {
    let session = SessionId(f.number("session_id")?);
    let holder = match f.opt_parse("agent", AgentKey::parse)? {
        None => Holder::Session(session),
        Some(agent) => Holder::Agent { session, agent },
    };
    Ok(WorktreeHold {
        worktree: f.parse("worktree", WorktreeName::parse)?,
        holder,
        host: f.parse("host", HostName::parse)?,
        purpose: f.parse("purpose", Purpose::parse)?,
        item: f.opt_number("item_id")?.map(ItemId),
        taken_at: f.time("taken_at")?,
        last_seen: f.time("last_seen")?,
    })
}

/// The pool of these holds; rows that break a pool's rules are a corrupt record.
fn pool(key: PoolKey, holds: Vec<WorktreeHold>) -> StoreResult<WorktreePool> {
    WorktreePool::new(key, holds).map_err(|_| {
        corrupt(format!(
            "{TABLE}: a worktree held twice, or a holder holding two worktrees, in one pool"
        ))
    })
}

/// The pools of rows ordered by their pool's key, each pool's holds together.
fn pools(rows: &[SqliteRow]) -> StoreResult<Vec<WorktreePool>> {
    let mut pools = Vec::new();
    let mut current: Option<(PoolKey, Vec<WorktreeHold>)> = None;
    for row in rows {
        let f = Fields::of(row, TABLE);
        let key = key(&f)?;
        let hold = hold(&f)?;
        match &mut current {
            Some((open, holds)) if *open == key => holds.push(hold),
            _ => {
                if let Some((done, holds)) = current.replace((key, vec![hold])) {
                    pools.push(pool(done, holds)?);
                }
            }
        }
    }
    if let Some((done, holds)) = current {
        pools.push(pool(done, holds)?);
    }
    Ok(pools)
}

#[async_trait]
impl WorktreeRows for SqliteTx {
    async fn worktree_pool(
        &mut self,
        project: ProjectId,
        key: &PoolKey,
    ) -> StoreResult<WorktreePool> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM worktree_holds WHERE project_id = ?1 AND member_id = ?2 AND machine = ?3 \
             AND pool = ?4 ORDER BY worktree"
        ))
        .bind(int(project.get())?)
        .bind(int(key.member.get())?)
        .bind(key.machine.as_str())
        .bind(key.pool.as_str());
        let holds = self
            .all(statement)
            .await?
            .iter()
            .map(|row| hold(&Fields::of(row, TABLE)))
            .collect::<StoreResult<_>>()?;
        pool(key.clone(), holds)
    }

    async fn worktree_pools(&mut self, project: ProjectId) -> StoreResult<Vec<WorktreePool>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            columns!(),
            " FROM worktree_holds WHERE project_id = ?1 \
             ORDER BY member_id, machine, pool, worktree"
        ))
        .bind(int(project.get())?);
        pools(&self.all(statement).await?)
    }

    async fn put_worktree_pool(
        &mut self,
        project: ProjectId,
        pool: &WorktreePool,
    ) -> StoreResult<()> {
        let project = int(project.get())?;
        let key = pool.key();
        let member = int(key.member.get())?;
        let clear = sqlx::query(
            "DELETE FROM worktree_holds WHERE project_id = ?1 AND member_id = ?2 \
             AND machine = ?3 AND pool = ?4",
        )
        .bind(project)
        .bind(member)
        .bind(key.machine.as_str())
        .bind(key.pool.as_str());
        self.run(clear).await?;
        for hold in pool.holds() {
            let item = hold.item.map(|item| int(item.get())).transpose()?;
            let statement = sqlx::query(concat!(
                "INSERT INTO worktree_holds (",
                columns!(),
                ") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
            ))
            .bind(project)
            .bind(member)
            .bind(key.machine.as_str())
            .bind(key.pool.as_str())
            .bind(hold.worktree.as_str())
            .bind(int(hold.holder.session().get())?)
            .bind(hold.holder.agent().map(AgentKey::as_str))
            .bind(hold.host.as_str())
            .bind(hold.purpose.as_str())
            .bind(item)
            .bind(nanos(hold.taken_at)?)
            .bind(nanos(hold.last_seen)?);
            self.run(statement).await?;
        }
        Ok(())
    }

    async fn busy_worktree_pools(&mut self) -> StoreResult<Vec<(ProjectId, PoolKey)>> {
        let statement = sqlx::query(
            "SELECT DISTINCT project_id, member_id, machine, pool FROM worktree_holds \
             ORDER BY project_id, member_id, machine, pool",
        );
        self.all(statement)
            .await?
            .iter()
            .map(|row| {
                let f = Fields::of(row, TABLE);
                Ok((ProjectId(f.number("project_id")?), key(&f)?))
            })
            .collect()
    }
}
