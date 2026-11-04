#![forbid(unsafe_code)]

use imanari_runtime as rt;
use rt::serde_json::{self, Value};
use rt::sqlx::{self, Row, sqlite::SqliteRow};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct Task {
    id: String,
    revision: i64,
    completed: bool,
    owner: String,
    title: String,
}

impl Task {
    fn from_row(row: &SqliteRow) -> Result<Task, rt::ApiError> {
        Ok(Task {
            id: row.try_get("id")?,
            revision: row.try_get("revision")?,
            completed: row.try_get::<i64, _>("completed")? != 0,
            owner: row.try_get("owner")?,
            title: row.try_get("title")?,
        })
    }

    fn json(row: &SqliteRow) -> Result<Value, rt::ApiError> {
        serde_json::to_value(Task::from_row(row)?)
            .map_err(|_| rt::ApiError::new(500, "INTERNAL", "internal error"))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateTask {
    title: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteTask {
    id: String,
    revision: i64,
}

fn parse<T: serde::de::DeserializeOwned>(input: rt::Input) -> Result<T, rt::ApiError> {
    serde_json::from_value(input.into_value())
        .map_err(|e| rt::ApiError::new(400, "INVALID_INPUT", e.to_string()))
}

const INSERT: &str = "INSERT INTO \"entity_Task\" (\"id\", \"revision\", \"completed\", \"owner\", \"title\") VALUES (?1, 1, 0, ?2, ?3) RETURNING \"id\", \"revision\", \"completed\", \"owner\", \"title\"";
const LIST: &str = "SELECT \"id\", \"revision\", \"completed\", \"owner\", \"title\" FROM \"entity_Task\" WHERE \"owner\" = ?1 AND (?2 IS NULL OR \"id\" > ?2) ORDER BY \"id\" ASC LIMIT ?3";
const UPDATE: &str = "UPDATE \"entity_Task\" SET \"completed\" = 1, \"revision\" = \"revision\" + 1 WHERE \"id\" = ?1 AND \"owner\" = ?2 AND \"revision\" = ?3 AND \"revision\" < 9007199254740991 RETURNING \"id\", \"revision\", \"completed\", \"owner\", \"title\"";

fn create_task(ctx: rt::Ctx, input: rt::Input) -> rt::HandlerFuture {
    Box::pin(async move {
        let req: CreateTask = parse(input)?;
        rt::check_text("title", &req.title, 1, 200)?;
        let hash = rt::input_hash("create_task", &[("title", Value::from(req.title.clone()))]);
        let mut tx = match ctx.begin_mutation(hash).await? {
            rt::Begin::Replay(v) => return Ok(v),
            rt::Begin::Fresh(tx) => tx,
        };
        let row = sqlx::query(INSERT)
            .bind(rt::new_uuid())
            .bind(ctx.actor())
            .bind(&req.title)
            .fetch_one(tx.conn())
            .await?;
        let data = Task::json(&row)?;
        ctx.commit_mutation(tx, data).await
    })
}

fn list_tasks(ctx: rt::Ctx, input: rt::Input) -> rt::HandlerFuture {
    Box::pin(async move {
        let (limit, after) = input.page()?;
        let rows = sqlx::query(LIST)
            .bind(ctx.actor())
            .bind(after)
            .bind(limit + 1)
            .fetch_all(ctx.db())
            .await?;
        rt::page(rows, limit, Task::json)
    })
}

fn complete_task(ctx: rt::Ctx, input: rt::Input) -> rt::HandlerFuture {
    Box::pin(async move {
        let req: CompleteTask = parse(input)?;
        if !rt::is_canonical_uuid(&req.id) {
            return Err(rt::ApiError::invalid(
                "id",
                "'id' must be a canonical lowercase UUID",
            ));
        }
        if req.revision.abs() > rt::MAX_SAFE_INT {
            return Err(rt::ApiError::invalid(
                "revision",
                "'revision' is outside the safe integer range",
            ));
        }
        let hash = rt::input_hash(
            "complete_task",
            &[
                ("id", Value::from(req.id.clone())),
                ("revision", Value::from(req.revision)),
            ],
        );
        let mut tx = match ctx.begin_mutation(hash).await? {
            rt::Begin::Replay(v) => return Ok(v),
            rt::Begin::Fresh(tx) => tx,
        };
        let row = sqlx::query(UPDATE)
            .bind(&req.id)
            .bind(ctx.actor())
            .bind(req.revision)
            .fetch_optional(tx.conn())
            .await?;
        match row {
            Some(row) => {
                let data = Task::json(&row)?;
                ctx.commit_mutation(tx, data).await
            }
            None => Err(ctx
                .update_miss(
                    tx,
                    "SELECT \"revision\" FROM \"entity_Task\" WHERE \"id\" = ?1 AND \"owner\" = ?2",
                    &req.id,
                    req.revision,
                )
                .await),
        }
    })
}

static SPEC: rt::AppSpec = rt::AppSpec {
    name: "fieldwork-baseline",
    source_sha256: "hand-written",
    semantic_sha256: "hand-written",
    schema: &[
        "CREATE TABLE \"entity_Task\" (\"id\" TEXT PRIMARY KEY NOT NULL, \"revision\" INTEGER NOT NULL CHECK (\"revision\" >= 1), \"completed\" INTEGER NOT NULL CHECK (\"completed\" IN (0, 1)), \"owner\" TEXT NOT NULL, \"title\" TEXT NOT NULL) STRICT",
        "CREATE INDEX \"entity_Task_owner\" ON \"entity_Task\" (\"owner\", \"id\")",
    ],
    schema_fingerprint: "fieldwork-baseline-v1",
    schema_shape: "[]",
    database: ".nari/baseline.db",
    port: 7332,
    request_limit_bytes: 65536,
    request_timeout_ms: 5000,
    routes: &[],
    index_html: "",
    assets: &[],
    actions: &[
        rt::ActionSpec {
            name: "complete_task",
            mutates: true,
            handler: complete_task,
        },
        rt::ActionSpec {
            name: "create_task",
            mutates: true,
            handler: create_task,
        },
        rt::ActionSpec {
            name: "list_tasks",
            mutates: false,
            handler: list_tasks,
        },
    ],
};

fn main() -> std::process::ExitCode {
    rt::run(&SPEC)
}
