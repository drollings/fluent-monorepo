use tokio::task::JoinSet;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

fn poll_join(set: Pin<&mut JoinSet<()>>, cx: &mut Context<'_>) -> Poll<Option<Result<(tokio::task::Id, ()), tokio::task::JoinError>>> {
    set.poll_join_next_with_id(cx)
}
