#![cfg(feature = "pg-tests")]
#![allow(clippy::panic)]

#[test]
fn test_database_url_must_be_set_for_pg_tests() {
    assert!(
        std::env::var("TEST_DATABASE_URL").is_ok(),
        "pg-tests feature is enabled but TEST_DATABASE_URL is unset; DB tests would skip silently. \
         Set TEST_DATABASE_URL to a Postgres the suite can create test databases against."
    );
}
