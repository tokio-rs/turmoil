/// Very simple tracing at `info` level.
///
/// Uses `target: "turmoil"` for subscriber filtering and routing.
#[macro_export]
macro_rules! trace {
    ( $($t:tt)* ) => {{
        tracing::info!(target: "turmoil", $($t)*)
    }};
}
