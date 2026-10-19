//! Threads, their messages, and what each member has read.

use std::collections::BTreeSet;

use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
use time::OffsetDateTime;

use dibs_app::ports::{AckRows, StoreResult, ThreadRows};
use dibs_app::records::{MessageRecord, ReadMark, ThreadRecord, ThreadSummary};
use dibs_domain::ids::{ItemId, MemberId, MessageId, ProjectId, ThreadId};
use dibs_domain::message::{Ack, Body, Importance, Recipients, SessionLabel, Subject};

use crate::SqliteTx;
use crate::codec::{Fields, int, name, nanos};

/// The columns of `messages m` a message is read from by [`message`].
macro_rules! message_columns {
    () => {
        "m.id, m.thread_id, m.author_id, m.label, m.body, m.importance, m.ack_required, \
         m.created_at"
    };
}

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
        importance: f.named("importance")?,
        ack_required: f.flag("ack_required")?,
        created_at: f.time("created_at")?,
    })
}

fn ack(row: &SqliteRow) -> StoreResult<Ack> {
    let f = Fields::of(row, "acks");
    Ok(Ack {
        message: MessageId(f.number("message_id")?),
        member: MemberId(f.number("member_id")?),
        at: f.time("at")?,
    })
}

fn summary(row: &SqliteRow) -> StoreResult<ThreadSummary> {
    let f = Fields::of(row, "threads");
    Ok(ThreadSummary {
        thread: thread(row)?,
        messages: f.number("messages")?,
        unread: f.number("unread")?,
        last_message_at: f.opt_time("last_message_at")?,
        importance: f
            .json::<Vec<Importance>>("importances")?
            .into_iter()
            .max()
            .unwrap_or_default(),
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
            "INSERT INTO messages (project_id, id, thread_id, author_id, label, body, importance, \
             ack_required, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(int(project.get())?)
        .bind(int(record.id.get())?)
        .bind(int(record.thread.get())?)
        .bind(int(record.author.get())?)
        .bind(record.label.as_str().to_owned())
        .bind(record.body.as_str().to_owned())
        .bind(name(&record.importance)?)
        .bind(record.ack_required)
        .bind(nanos(record.created_at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn message(
        &mut self,
        project: ProjectId,
        id: MessageId,
    ) -> StoreResult<Option<MessageRecord>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            message_columns!(),
            " FROM messages m WHERE m.project_id = ?1 AND m.id = ?2"
        ))
        .bind(int(project.get())?)
        .bind(int(id.get())?);
        self.optional(statement)
            .await?
            .as_ref()
            .map(message)
            .transpose()
    }

    async fn messages(
        &mut self,
        project: ProjectId,
        thread: ThreadId,
    ) -> StoreResult<Vec<MessageRecord>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            message_columns!(),
            " FROM messages m WHERE m.project_id = ?1 AND m.thread_id = ?2 \
             ORDER BY m.created_at, m.id"
        ))
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
              WHERE m.project_id = t.project_id AND m.thread_id = t.id) AS last_message_at, \
             (SELECT json_group_array(DISTINCT m.importance) FROM messages m \
              WHERE m.project_id = t.project_id AND m.thread_id = t.id) AS importances \
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
             WHERE m.project_id = ?1 AND m.thread_id = ?3 AND m.author_id <> ?2 \
             ON CONFLICT (project_id, member_id, message_id) DO NOTHING",
        )
        .bind(int(project.get())?)
        .bind(int(reader.get())?)
        .bind(int(thread.get())?)
        .bind(nanos(at)?);
        self.run(statement).await
    }
}

#[async_trait]
impl AckRows for SqliteTx {
    async fn insert_ack(&mut self, project: ProjectId, record: &Ack) -> StoreResult<()> {
        let statement = sqlx::query(
            "INSERT INTO acks (project_id, message_id, member_id, at) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(int(project.get())?)
        .bind(int(record.message.get())?)
        .bind(int(record.member.get())?)
        .bind(nanos(record.at)?);
        self.run(statement).await?;
        Ok(())
    }

    async fn acks(&mut self, project: ProjectId, message: MessageId) -> StoreResult<Vec<Ack>> {
        let statement = sqlx::query(
            "SELECT message_id, member_id, at FROM acks \
             WHERE project_id = ?1 AND message_id = ?2 ORDER BY at, member_id",
        )
        .bind(int(project.get())?)
        .bind(int(message.get())?);
        self.all(statement).await?.iter().map(ack).collect()
    }

    async fn thread_acks(&mut self, project: ProjectId, thread: ThreadId) -> StoreResult<Vec<Ack>> {
        let statement = sqlx::query(
            "SELECT a.message_id, a.member_id, a.at FROM acks a \
             JOIN messages m ON m.project_id = a.project_id AND m.id = a.message_id \
             WHERE a.project_id = ?1 AND m.thread_id = ?2 \
             ORDER BY a.at, a.message_id, a.member_id",
        )
        .bind(int(project.get())?)
        .bind(int(thread.get())?);
        self.all(statement).await?.iter().map(ack).collect()
    }

    async fn ack_requests(
        &mut self,
        project: ProjectId,
        member: MemberId,
    ) -> StoreResult<Vec<MessageRecord>> {
        let statement = sqlx::query(concat!(
            "SELECT ",
            message_columns!(),
            " FROM messages m \
             WHERE m.project_id = ?1 AND m.ack_required = 1 AND m.author_id <> ?2 \
             AND NOT EXISTS (SELECT 1 FROM acks a WHERE a.project_id = m.project_id \
                 AND a.message_id = m.id AND a.member_id = ?2) \
             ORDER BY m.created_at DESC, m.id DESC"
        ))
        .bind(int(project.get())?)
        .bind(int(member.get())?);
        self.all(statement).await?.iter().map(message).collect()
    }

    async fn reads_of(
        &mut self,
        project: ProjectId,
        message: MessageId,
    ) -> StoreResult<Vec<ReadMark>> {
        let statement = sqlx::query(
            "SELECT member_id, read_at FROM reads \
             WHERE project_id = ?1 AND message_id = ?2 ORDER BY member_id",
        )
        .bind(int(project.get())?)
        .bind(int(message.get())?);
        self.all(statement)
            .await?
            .iter()
            .map(|row| {
                let f = Fields::of(row, "reads");
                Ok(ReadMark {
                    member: MemberId(f.number("member_id")?),
                    at: f.time("read_at")?,
                })
            })
            .collect()
    }
}
