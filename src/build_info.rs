pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUILD_TAG: &str = match option_env!("BUILD_TAG") {
    Some(value) => value,
    None => "dev",
};
pub const GIT_SHA: &str = match option_env!("GIT_SHA") {
    Some(value) => value,
    None => "dev",
};
