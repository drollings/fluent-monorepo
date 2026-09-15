//! The `OwnedValue` bind path: type matrix.

use fluent_wvr::capability::CURRENT_CAPS;

use crate::query::OwnedValue;
use crate::tests::common::{db_caps, in_memory_pool};

const SCHEMA: &str = "CREATE TABLE m (t TEXT, i INTEGER, r REAL, b BLOB, n TEXT)";

#[tokio::test]
async fn owned_value_type_matrix_round_trips() {
    CURRENT_CAPS
        .scope(db_caps(), async {
            let pool = in_memory_pool();
            pool.execute_batch(SCHEMA).await.unwrap();
            pool.execute(
                "INSERT INTO m VALUES (?1, ?2, ?3, ?4, ?5)",
                vec![
                    OwnedValue::from("hello"),
                    OwnedValue::from(42_i64),
                    OwnedValue::from(2.5_f64),
                    OwnedValue::from(vec![1_u8, 2, 3]),
                    OwnedValue::Null,
                ],
            )
            .await
            .unwrap();
            // `From` ergonomics across the common call-site types.
            pool.execute(
                "INSERT INTO m VALUES (?1, ?2, ?3, ?4, ?5)",
                [
                    OwnedValue::from(true),
                    OwnedValue::from(7_i32),
                    OwnedValue::from(1.5_f32),
                    OwnedValue::from(Option::<Vec<u8>>::None),
                    OwnedValue::from(Option::<String>::None),
                ],
            )
            .await
            .unwrap();

            let rows = pool
                .query_rows(
                    "SELECT t, i, r, b, n FROM m ORDER BY rowid",
                    Vec::<OwnedValue>::new(),
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, f64>(2)?,
                            row.get::<_, Option<Vec<u8>>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ))
                    },
                )
                .await
                .unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(
                rows[0],
                (
                    "hello".to_string(),
                    42,
                    2.5,
                    Some(vec![1_u8, 2, 3]),
                    None
                )
            );
            // `bool` binds as integer 1 (TEXT affinity reads it back as "1").
            assert_eq!(
                rows[1],
                ("1".to_string(), 7, 1.5, None, None)
            );

            // Borrowed params (`&str`, `String`, integers) convert directly.
            let count = pool
                .query_row(
                    "SELECT COUNT(*) FROM m WHERE t = ?1",
                    ["hello"],
                    |row| row.get::<_, i64>(0),
                )
                .await
                .unwrap();
            assert_eq!(count, Some(1));
        })
        .await;
}
