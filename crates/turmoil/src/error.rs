/// A specialized [`Result`] type for turmoil simulations.
///
/// This type is generally useful for fallible test cases, i.e. where you want
/// to use the `?` operator to fail the test rather than writing unwrap
/// everywhere.
///
/// [`Result`]: std::result::Result
pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
