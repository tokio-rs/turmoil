/// Tracing via delegating to [`tracing::event!`] at `INFO` level.
///
/// Uses `target: "turmoil"` for subscriber filtering and routing.
#[macro_export]
macro_rules! trace {
    ({ $($field:tt)+ }, $($arg:tt)+ ) => (
        tracing::event!(
            target: "turmoil",
            tracing::Level::INFO,
            { $($field)+ },
            $($arg)+
        )
    );
    ($($k:ident).+ = $($field:tt)*) => (
        tracing::event!(
            target: "turmoil",
            tracing::Level::INFO,
            { $($k).+ = $($field)*}
        )
    );
    (?$($k:ident).+ = $($field:tt)*) => (
        tracing::event!(
            target: "turmoil",
            tracing::Level::INFO,
            { ?$($k).+ = $($field)*}
        )
    );
    (%$($k:ident).+ = $($field:tt)*) => (
        tracing::event!(
            target: "turmoil",
            tracing::Level::INFO,
            { %$($k).+ = $($field)*}
        )
    );
    ($($k:ident).+, $($field:tt)*) => (
        tracing::event!(
            target: "turmoil",
            tracing::Level::INFO,
            { $($k).+, $($field)*}
        )
    );
    (?$($k:ident).+, $($field:tt)*) => (
        tracing::event!(
            target: "turmoil",
            tracing::Level::INFO,
            { ?$($k).+, $($field)*}
        )
    );
    (%$($k:ident).+, $($field:tt)*) => (
        tracing::event!(
            target: "turmoil",
            tracing::Level::INFO,
            { %$($k).+, $($field)*}
        )
    );
    (?$($k:ident).+) => (
        tracing::event!(
            target: "turmoil",
            tracing::Level::INFO,
            { ?$($k).+ }
        )
    );
    (%$($k:ident).+) => (
        tracing::event!(
            target: "turmoil",
            tracing::Level::INFO,
            { %$($k).+ }
        )
    );
    ($($k:ident).+) => (
        tracing::event!(
            target: "turmoil",
            tracing::Level::INFO,
            { $($k).+ }
        )
    );
    ($($arg:tt)+) => (
        tracing::event!(
            target: "turmoil",
            tracing::Level::INFO,
            {},
            $($arg)+
        )
    );
}
