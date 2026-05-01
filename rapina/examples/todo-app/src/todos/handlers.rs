use rapina::database::{Db, DbError};
use rapina::pagination::{CursorPaginate, CursorPaginated};
use rapina::prelude::*;
use rapina::sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, PaginatorTrait, Set};

use crate::entity::Todo;
use crate::entity::todo::{ActiveModel, Model};

use super::dto::{CreateTodo, UpdateTodo};
use super::error::TodoError;

use crate::AppConfig;

#[get("/todos")]
#[errors(TodoError)]
pub async fn list_todos(db: Db) -> Result<Json<Vec<Model>>> {
    let todos = Todo::find().all(db.conn()).await.map_err(DbError)?;
    Ok(Json(todos))
}

#[get("/todos/cursor")]
#[errors(TodoError)]
pub async fn list_todos_cursor(
    db: Db,
    cursor: CursorPaginate<i32>,
) -> Result<CursorPaginated<Model>> {
    cursor.exec(Todo::find(), db.conn()).await
}

#[get("/todos/:id")]
#[errors(TodoError)]
pub async fn get_todo(db: Db, id: Path<i32>) -> Result<Json<Model>> {
    let id = *id;
    let todo = Todo::find_by_id(id)
        .one(db.conn())
        .await
        .map_err(DbError)?
        .ok_or_else(|| Error::not_found(format!("Todo {} not found", id)))?;
    Ok(Json(todo))
}

#[post("/todos")]
#[errors(TodoError)]
pub async fn create_todo(
    db: Db,
    config: State<AppConfig>,
    body: Validated<Json<CreateTodo>>,
) -> Result<Json<Model>> {
    let todo_current_count = Todo::find().count(db.conn()).await.map_err(DbError)?;

    if todo_current_count as u32 >= config.max_todos {
        return Err(Error::bad_request("Todo limit reached"));
    };

    let body = body.into_inner();

    let todo = ActiveModel {
        title: Set(body.title.clone()),
        ..Default::default()
    };
    let result = todo.insert(db.conn()).await.map_err(DbError)?;
    Ok(Json(result))
}

#[put("/todos/:id")]
#[errors(TodoError)]
pub async fn update_todo(db: Db, id: Path<i32>, body: Json<UpdateTodo>) -> Result<Json<Model>> {
    let id = *id;
    let todo = Todo::find_by_id(id)
        .one(db.conn())
        .await
        .map_err(DbError)?
        .ok_or_else(|| Error::not_found(format!("Todo {} not found", id)))?;

    let mut active: ActiveModel = todo.into_active_model();
    if let Some(title) = body.title.clone() {
        active.title = Set(title);
    }
    if let Some(done) = body.done {
        active.done = Set(done);
    }

    let result = active.update(db.conn()).await.map_err(DbError)?;
    Ok(Json(result))
}

#[delete("/todos/:id")]
#[errors(TodoError)]
pub async fn delete_todo(db: Db, id: Path<i32>) -> Result<Json<serde_json::Value>> {
    let id = *id;
    let result = Todo::delete_by_id(id)
        .exec(db.conn())
        .await
        .map_err(DbError)?;
    if result.rows_affected == 0 {
        return Err(Error::not_found(format!("Todo {} not found", id)));
    }
    Ok(Json(serde_json::json!({ "deleted": id })))
}
