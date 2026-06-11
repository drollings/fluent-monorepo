#[allow(dead_code)]
fn check() {
    let handle = tokio::spawn(async {});
    let _id = handle.id();
    let abort = handle.abort_handle();
    let _id2 = abort.id();
}
