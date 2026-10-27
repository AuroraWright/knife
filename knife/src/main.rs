//! The `knife` executable; the program itself is the `knife_app` library, which Android loads as well.

fn main() -> anyhow::Result<()> {
    knife_app::run()
}
