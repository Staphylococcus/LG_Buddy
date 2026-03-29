mod cucumber_support;
mod support;

use cucumber::World as _;
use cucumber_support::world::LgBuddyWorld;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    LgBuddyWorld::cucumber()
        .max_concurrent_scenarios(1)
        .run_and_exit(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/features"))
        .await;
}
