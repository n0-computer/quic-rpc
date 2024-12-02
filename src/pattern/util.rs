//! Utility functions for working with streams
use futures_lite::{Stream, StreamExt};

/// Flatten a stream of results of results into a stream of results
///
/// This is frequently useful for streaming responses if you want to combine
/// the application errors with the transport errors.
pub fn flatten<T, E1, E2, E3>(
    s: impl Stream<Item = Result<Result<T, E1>, E2>>,
) -> impl Stream<Item = Result<T, E3>>
where
    E1: Into<E3> + Send + Sync + 'static,
    E2: Into<E3> + Send + Sync + 'static,
{
    s.map(|res| match res {
        Ok(Ok(res)) => Ok(res),
        Ok(Err(err)) => Err(err.into()),
        Err(err) => Err(err.into()),
    })
}
