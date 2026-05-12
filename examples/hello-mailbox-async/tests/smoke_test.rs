#[tokio::test]
async fn test_crate_compiles_and_runs() {
    // Smoke test: verify the async example crate builds and the tokio runtime starts
    let result = tokio::spawn(async { 1 + 1 }).await;
    assert_eq!(result.unwrap(), 2);
}

#[tokio::test]
async fn test_tokio_runtime_is_async() {
    // Edge case: ensure multiple concurrent tasks resolve correctly
    let (a, b) = tokio::join!(
        async { 10 },
        async { 20 },
    );
    assert_eq!(a + b, 30);
}
