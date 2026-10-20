//! Footprints with their paths, and the overlaps found between claims.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::sqlite::SqliteRow;

use dibs_app::ports::{RadarRows, StoreResult};
use dibs_domain::area::QualifiedArea;
use dibs_domain::ids::{ItemId, ProjectId};
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::radar::{
    BranchName, CommitSha, Footprint, FootprintBase, FootprintDigest, FootprintEntry, MergeMark,
    Overlap, OverlapKey, OverlapKind,
};
use dibs_domain::repo::RepoId;
use dibs_domain::review::PrRef;

use crate::SqliteTx;
use crate::codec::{Fields, int, json, nanos};

/// The columns a footprint is read from.
macro_rules! footprint_columns {
    () => {
        "item_id, repo, branch, base_branch, base_sha, base_at, truncated, path_count, digest"
    };
}

/// A digest as its column holds it.
#[derive(Serialize, Deserialize)]
struct DigestColumn {
    areas: BTreeSet<QualifiedArea>,
    unzoned: bool,
    contracts: BTreeSet<RelPath>,
}

impl From<&FootprintDigest> for DigestColumn {
    fn from(digest: &FootprintDigest) -> Self {
        Self {
            areas: digest.areas.clone(),
            unzoned: digest.unzoned,
            contracts: digest.contracts.clone(),
        }
    }
}

fn footprint(row: &SqliteRow) -> StoreResult<Footprint> {
    let f = Fields::of(row, "footprints");
    let digest: DigestColumn = f.json("digest")?;
    Ok(Footprint {
        item: ItemId(f.number("item_id")?),
        repo: f.parse("repo", RepoId::parse)?,
        branch: f.parse("branch", BranchName::parse)?,
        base: FootprintBase {
            branch: f.parse("base_branch", BranchName::parse)?,
            sha: f.parse("base_sha", CommitSha::parse)?,
            at: f.time("base_at")?,
        },
        truncated: f.flag("truncated")?,
        path_count: usize::try_from(f.number("path_count")?)
            .map_err(|error| f.bad("path_count", error))?,
        digest: FootprintDigest {
            areas: digest.areas,
            unzoned: digest.unzoned,
            contracts: digest.contracts,
        },
    })
}

fn entry(row: &SqliteRow, repo: &RepoId) -> StoreResult<FootprintEntry> {
    let f = Fields::of(row, "footprint_paths");
    Ok(FootprintEntry {
        item: ItemId(f.number("item_id")?),
        path: RepoPath {
            repo: repo.clone(),
            path: f.parse("path", RelPath::parse)?,
        },
    })
}

fn overlap(row: &SqliteRow) -> StoreResult<Overlap> {
    let f = Fields::of(row, "overlaps");
    let kind = f.parse("kind", |text| {
        OverlapKind::parse(text).ok_or("not a kind of overlap")
    })?;
    let items: BTreeSet<ItemId> = f.json("items")?;
    let merge = match (f.opt_parse("pr", PrRef::parse)?, f.opt_time("merged_at")?) {
        (Some(pr), Some(at)) => Some(MergeMark { pr, at }),
        (None, None) => None,
        _ => {
            return Err(f.bad(
                "merged_at",
                "a merge has its pull request and its time or neither",
            ));
        }
    };
    let paths: BTreeSet<RepoPath> = f.json("paths")?;
    Overlap::new(kind, items, merge, paths).map_err(|error| f.bad("items", error))
}

#[async_trait]
impl RadarRows for SqliteTx {
    async fn footprints(&mut self, project: ProjectId) -> StoreResult<Vec<Footprint>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            footprint_columns!(),
            " FROM footprints WHERE project_id = ?1 ORDER BY item_id, repo"
        ))
        .bind(int(project.get())?);
        self.all(statement).await?.iter().map(footprint).collect()
    }

    async fn footprint(
        &mut self,
        project: ProjectId,
        item: ItemId,
        repo: &RepoId,
    ) -> StoreResult<Option<(Footprint, BTreeSet<RelPath>)>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            footprint_columns!(),
            " FROM footprints WHERE project_id = ?1 AND item_id = ?2 AND repo = ?3"
        ))
        .bind(int(project.get())?)
        .bind(int(item.get())?)
        .bind(repo.as_str().to_owned());
        let Some(found) = self.optional(statement).await?.as_ref().map(footprint) else {
            return Ok(None);
        };
        let found = found?;
        let statement = sqlx::query(
            "SELECT item_id, path FROM footprint_paths \
             WHERE project_id = ?1 AND item_id = ?2 AND repo = ?3 ORDER BY path",
        )
        .bind(int(project.get())?)
        .bind(int(item.get())?)
        .bind(repo.as_str().to_owned());
        let paths = self
            .all(statement)
            .await?
            .iter()
            .map(|row| entry(row, repo).map(|entry| entry.path.path))
            .collect::<StoreResult<BTreeSet<RelPath>>>()?;
        Ok(Some((found, paths)))
    }

    async fn save_footprint(
        &mut self,
        project: ProjectId,
        footprint: &Footprint,
        paths: &BTreeSet<RelPath>,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO footprints (project_id, item_id, repo, branch, base_branch, base_sha, \
             base_at, truncated, path_count, digest) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT (project_id, item_id, repo) DO UPDATE SET branch = excluded.branch, \
             base_branch = excluded.base_branch, base_sha = excluded.base_sha, \
             base_at = excluded.base_at, truncated = excluded.truncated, \
             path_count = excluded.path_count, digest = excluded.digest",
        )
        .bind(int(project.get())?)
        .bind(int(footprint.item.get())?)
        .bind(footprint.repo.as_str().to_owned())
        .bind(footprint.branch.as_str().to_owned())
        .bind(footprint.base.branch.as_str().to_owned())
        .bind(footprint.base.sha.as_str().to_owned())
        .bind(nanos(footprint.base.at)?)
        .bind(i64::from(footprint.truncated))
        .bind(int(u64::try_from(paths.len()).unwrap_or(u64::MAX))?)
        .bind(json(&DigestColumn::from(&footprint.digest))?);
        self.run(statement).await?;
        let statement = sqlx::query(
            "DELETE FROM footprint_paths WHERE project_id = ?1 AND item_id = ?2 AND repo = ?3",
        )
        .bind(int(project.get())?)
        .bind(int(footprint.item.get())?)
        .bind(footprint.repo.as_str().to_owned());
        self.run(statement).await?;
        let statement = sqlx::query(
            "INSERT INTO footprint_paths (project_id, item_id, repo, path) \
             SELECT ?1, ?2, ?3, value FROM json_each(?4)",
        )
        .bind(int(project.get())?)
        .bind(int(footprint.item.get())?)
        .bind(footprint.repo.as_str().to_owned())
        .bind(json(paths)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn set_footprint_digest(
        &mut self,
        project: ProjectId,
        item: ItemId,
        repo: &RepoId,
        digest: &FootprintDigest,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "UPDATE footprints SET digest = ?4 WHERE project_id = ?1 AND item_id = ?2 AND repo = ?3",
        )
        .bind(int(project.get())?)
        .bind(int(item.get())?)
        .bind(repo.as_str().to_owned())
        .bind(json(&DigestColumn::from(digest))?);
        self.run(statement).await?;
        Ok(())
    }

    async fn delete_footprint(
        &mut self,
        project: ProjectId,
        item: ItemId,
        repo: &RepoId,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "DELETE FROM footprint_paths WHERE project_id = ?1 AND item_id = ?2 AND repo = ?3",
        )
        .bind(int(project.get())?)
        .bind(int(item.get())?)
        .bind(repo.as_str().to_owned());
        self.run(statement).await?;
        let statement = sqlx::query(
            "DELETE FROM footprints WHERE project_id = ?1 AND item_id = ?2 AND repo = ?3",
        )
        .bind(int(project.get())?)
        .bind(int(item.get())?)
        .bind(repo.as_str().to_owned());
        self.run(statement).await?;
        Ok(())
    }

    async fn delete_footprints(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()> {
        let statement =
            sqlx::query("DELETE FROM footprint_paths WHERE project_id = ?1 AND item_id = ?2")
                .bind(int(project.get())?)
                .bind(int(item.get())?);
        self.run(statement).await?;
        let statement =
            sqlx::query("DELETE FROM footprints WHERE project_id = ?1 AND item_id = ?2")
                .bind(int(project.get())?)
                .bind(int(item.get())?);
        self.run(statement).await?;
        Ok(())
    }

    async fn footprint_entries(
        &mut self,
        project: ProjectId,
        repo: &RepoId,
        paths: &BTreeSet<RelPath>,
    ) -> StoreResult<Vec<FootprintEntry>> {
        // The paths asked for drive the join, so each is looked up through the index by path
        // rather than every path of the project being read.
        let statement = sqlx::query(
            "SELECT fp.item_id, fp.path FROM json_each(?3) AS asked \
             CROSS JOIN footprint_paths AS fp \
             WHERE fp.project_id = ?1 AND fp.repo = ?2 AND fp.path = asked.value \
             ORDER BY fp.item_id, fp.path",
        )
        .bind(int(project.get())?)
        .bind(repo.as_str().to_owned())
        .bind(json(paths)?);
        self.all(statement)
            .await?
            .iter()
            .map(|row| entry(row, repo))
            .collect()
    }

    async fn overlaps(&mut self, project: ProjectId) -> StoreResult<Vec<Overlap>> {
        let statement = sqlx::query(
            "SELECT o.kind, o.pr, o.merged_at, o.paths, \
             (SELECT json_group_array(i.item_id) FROM overlap_items i \
              WHERE i.project_id = o.project_id AND i.overlap_key = o.overlap_key) AS items \
             FROM overlaps o WHERE o.project_id = ?1",
        )
        .bind(int(project.get())?);
        let mut found = self
            .all(statement)
            .await?
            .iter()
            .map(overlap)
            .collect::<StoreResult<Vec<Overlap>>>()?;
        found.sort_by_key(Overlap::key);
        Ok(found)
    }

    async fn save_overlap(&mut self, project: ProjectId, overlap: &Overlap) -> StoreResult<()> {
        let key = overlap.key().to_string();
        let statement = sqlx::query(
            "INSERT INTO overlaps (project_id, overlap_key, kind, pr, merged_at, paths) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (project_id, overlap_key) DO UPDATE SET kind = excluded.kind, \
             pr = excluded.pr, merged_at = excluded.merged_at, paths = excluded.paths",
        )
        .bind(int(project.get())?)
        .bind(key.clone())
        .bind(overlap.kind().code())
        .bind(overlap.pr().map(|pr| pr.as_str().to_owned()))
        .bind(overlap.merge().map(|merge| nanos(merge.at)).transpose()?)
        .bind(json(overlap.paths())?);
        self.run(statement).await?;
        for item in overlap.items() {
            let statement = sqlx::query(
                "INSERT INTO overlap_items (project_id, overlap_key, item_id) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (project_id, overlap_key, item_id) DO NOTHING",
            )
            .bind(int(project.get())?)
            .bind(key.clone())
            .bind(int(item.get())?);
            self.run(statement).await?;
        }
        Ok(())
    }

    async fn delete_overlap(&mut self, project: ProjectId, key: &OverlapKey) -> StoreResult<()> {
        let statement =
            sqlx::query("DELETE FROM overlap_items WHERE project_id = ?1 AND overlap_key = ?2")
                .bind(int(project.get())?)
                .bind(key.to_string());
        self.run(statement).await?;
        let statement =
            sqlx::query("DELETE FROM overlaps WHERE project_id = ?1 AND overlap_key = ?2")
                .bind(int(project.get())?)
                .bind(key.to_string());
        self.run(statement).await?;
        Ok(())
    }

    async fn delete_overlaps_of(&mut self, project: ProjectId, item: ItemId) -> StoreResult<()> {
        let statement = sqlx::query(
            "DELETE FROM overlaps WHERE project_id = ?1 AND overlap_key IN \
             (SELECT overlap_key FROM overlap_items WHERE project_id = ?1 AND item_id = ?2)",
        )
        .bind(int(project.get())?)
        .bind(int(item.get())?);
        self.run(statement).await?;
        let statement = sqlx::query(
            "DELETE FROM overlap_items WHERE project_id = ?1 AND NOT EXISTS \
             (SELECT 1 FROM overlaps o WHERE o.project_id = overlap_items.project_id \
              AND o.overlap_key = overlap_items.overlap_key)",
        )
        .bind(int(project.get())?);
        self.run(statement).await?;
        Ok(())
    }
}
