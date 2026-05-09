// reqwest's transitive dependency tree pulls in two versions of
// `getrandom` and `windows-sys`. Neither is something we can resolve
// without upstream changes, so allow the clippy lint at the crate root.
#![allow(clippy::multiple_crate_versions)]

fn main() -> anyhow::Result<()> {
    prodder::real_main()
}
