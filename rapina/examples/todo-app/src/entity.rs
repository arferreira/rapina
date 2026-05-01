use rapina::pagination::CursorKey;
use rapina::prelude::*;

schema! {
    #[timestamps(none)]
    Todo {
        title: String,
        done: bool,
    }
}

impl CursorKey for todo::Model {
    type Column = todo::Column;
    type Value = i32;
    const COLUMN: todo::Column = todo::Column::Id;
    fn cursor_value(&self) -> i32 {
        self.id
    }
}
