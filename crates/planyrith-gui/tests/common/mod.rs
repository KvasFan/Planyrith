//! Locates workspace binaries for desktop frontend acceptance (docs/12).

/// A separately built program in Cargo's profile directory, across both
/// the `deps` and `build/<package>/<hash>/out` executable layouts.
pub fn sibling(name: &str) -> std::path::PathBuf {
    let executable = std::env::current_exe().unwrap();
    let parent = executable.parent().unwrap();
    let build = parent.ancestors().nth(3);
    let profile = if parent.ends_with("deps") {
        parent.parent().unwrap()
    } else if parent.ends_with("out") && build.is_some_and(|path| path.ends_with("build")) {
        build.unwrap().parent().unwrap()
    } else {
        parent
    };
    profile.join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}
