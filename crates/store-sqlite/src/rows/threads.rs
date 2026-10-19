//! Threads, their messages, and what each member has read.

use std::collections::BTreeSet;

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;

use dibs_app::ports::{StoreResult, ThreadRows};
use dibs_app::records::{MessageRecord, ThreadRecord, ThreadSummary};
use dibs_domain::ids::{ItemId, MemberId, MessageId, ProjectId, ThreadId};
use dibs_domain::message::{Body, Recipients, SessionLabel, Subject};

use crate::SqliteTx;
use crate::codec::{Fields, int, nanos};

/// The recipients as the thread row's flag and the members listed beside it.
fn recipient_rows(recipients: &Recipients) -> (bool, Vec<MemberId>) {
    match recipients {
        Recipients::Project => (true, Vec::new()),
        Recipients::Members(members) => (false, members.iter().copied().collect()),
    }
}

fn recipients(f: &Fields<'_>) -> StoreResult<Recipients> {
    let to_project = f.flag("to_project")?;
    let members: BTreeSet<MemberId> = f
        .json::<Vec<u64>>("recipients")?
        .into_iter()
        .map(MemberId)
        .collect();
    if to_project {
        return if members.is_empty() {
            Ok(Recipients::Project)
        } else {
            Err(f.bad("recipients", "members listed beside the whole project"))
        };
    }
    Recipients::members(members).map_err(|error| f.bad("recipients", error))
}

fn thread(row: &SqliteRow) -> StoreResult<ThreadRecord> {
    let f = Fields::of(row, "threads");
    Ok(ThreadRecord {
        id: ThreadId(f.number("id")?),
        subject: f.parse("subject", Subject::parse)?,
        item: f.opt_number("item_id")?.map(ItemId),
        author: MemberId(f.number("author_id")?),
        recipients: recipients(&f)?,
        created_at: f.time("created_at")?,
    })
}

fn message(row: &SqliteRow) -> StoreResult<MessageRecord> {
    let f = Fields::of(row, "messages");
    Ok(MessageRecord {
        id: MessageId(f.number("id")?),
        thread: ThreadId(f.number("thread_id")?),
        author: MemberId(f.number("author_id")?),
        label: f.parse("label", SessionLabel::parse)?,
        body: f.parse("body", Body::parse)?,
        created_at: f.time("created_at")?,
    })
}

fn summary(row: &SqliteRow) -> StoreResult<ThreadSummary> {
    let f = Fields::of(row, "threads");
    Ok(ThreadSummary {
        thread: thread(row)?,
        messages: f.number("messages")?,
        unread: f.number("unread")?,
        last_message_at: f.opt_time("last_message_at")?,
    })
}

#[async_trait]
impl ThreadRows for SqliteTx {
    async fn insert_thread(
        &mut self,
        project: ProjectId,
        record: &ThreadRecord,
    ) -> StoreResult<()> {
        let (to_project, members) = recipient_rows(&record.recipients);
        let statement = sqlx::query(
            "INSERT INTO threads (project_id, id, subject, item_id, author_id, to_project, \
             created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(int(project.get())?)
        .bind(int(record.id.get())?)
        .bind(record.subject.as_str().to_owned())
        .bind(record.item.map(|item| int(item.get())).transpose()?)
        .bind(int(record.author.get())?)
        .bind(to_project)
        .bind(nanos(record.created_at)?);
        self.run(statement).await?;
        for member in members {
            let statement = sqlx::query(
                "INSERT INTO thread_recipients (project_id, thread_id, member_id) \
                 VALUES (?1, ?2, ?3)",
            )
            .bind(int(project.get())?)
            .bind(int(record.id.get())?)
            .bind(int(member.get())?);
            self.run(statement).await?;
        }
        Ok(())
    }

    async fn thread(
        &mut self,
        project: ProjectId,
        id: ThreadId,
    ) -> StoreResult<Option<ThreadRecord>> {
        let statement = sqlx::query(
            "SELECT t.id, t.subject, t.item_id, t.author_id, t.to_project, t.created_at, \
             (SELECT json_group_array(r.member_id) FROM thread_recipients r \
              WHERE r.project_id = t.project_id AND r.thread_id = t.id) AS recipients \
             FROM threads t WHERE t.project_id = ?1 AND t.id = ?2",
        )
        .bind(int(project.get())?)
        .bind(int(id.get())?);
        self.optional(statement)
            .await?
            .as_ref()
            .map(thread)
            .transpose()
    }

    async fn insert_message(
        &mut self,
        project: ProjectId,
        record: &MessageRecord,
    ) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO messages (project_id, id, thread_id, author_id, label, body, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(int(project.get())?)
        .bind(int(record.id.get())?)
        .bind(int(record.thread.get())?)
        .bind(int(record.author.get())?)
        .bind(record.label.as_str().to_owned())
        .bind(record.body.as_str().to_owned())
        .bind(nanos(record.created_at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn messages(
        &mut self,
        project: ProjectId,
        thread: ThreadId,
    ) -> StoreResult<Vec<MessageRecord>> {
        let statement = sqlx::query(
            "SELECT id, thread_id, author_id, label, body, created_at FROM messages \
             WHERE project_id = ?1 AND thread_id = ?2 ORDER BY created_at, id",
        )
        .bind(int(project.get())?)
        .bind(int(thread.get())?);
        self.all(statement).await?.iter().map(message).collect()
    }

    async fn thread_summaries(
        &mut self,
        project: ProjectId,
        reader: MemberId,
        item: Option<ItemId>,
    ) -> StoreResult<Vec<ThreadSummary>> {
        let statement = sqlx::query(
            "SELECT t.id, t.subject, t.item_id, t.author_id, t.to_project, t.created_at, \
             (SELECT json_group_array(r.member_id) FROM thread_recipients r \
              WHERE r.project_id = t.project_id AND r.thread_id = t.id) AS recipients, \
             (SELECT COUNT(*) FROM messages m \
              WHERE m.project_id = t.project_id AND m.thread_id = t.id) AS messages, \
             (SELECT COUNT(*) FROM messages m \
              WHERE m.project_id = t.project_id AND m.thread_id = t.id AND m.author_id <> ?2 \
              AND NOT EXISTS (SELECT 1 FROM reads d WHERE d.project_id = m.project_id \
                  AND d.member_id = ?2 AND d.message_id = m.id)) AS unread, \
             (SELECT MAX(m.created_at) FROM messages m \
              WHERE m.project_id = t.project_id AND m.thread_id = t.id) AS last_message_at \
             FROM threads t WHERE t.project_id = ?1 AND (?3 IS NULL OR t.item_id = ?3) \
             ORDER BY COALESCE(last_message_at, t.created_at) DESC, t.id DESC",
        )
        .bind(int(project.get())?)
        .bind(int(reader.get())?)
        .bind(item.map(|item| int(item.get())).transpose()?);
        self.all(statement).await?.iter().map(summary).collect()
    }

    async fn mark_read(
        &mut self,
        project: ProjectId,
        reader: MemberId,
        thread: ThreadId,
        at: OffsetDateTime,
    ) -> StoreResult<u64> {
        let statement = sqlx::query(
            "INSERT INTO reads (project_id, member_id, message_id, read_at) \
             SELECT m.project_id, ?2, m.id, ?4 FROM messages m \
             WHERE m.project_id = ?1 AND m.thread_id = ?3 \
             ON CONFLICT (project_id, member_id, message_id) DO NOTHING",
        )
        .bind(int(project.get())?)
        .bind(int(reader.get())?)
        .bind(int(thread.get())?)
        .bind(nanos(at)?);
        self.run(statement).await
    }
}
