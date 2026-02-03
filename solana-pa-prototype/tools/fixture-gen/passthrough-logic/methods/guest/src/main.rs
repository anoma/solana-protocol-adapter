use arm::logic_instance::LogicInstance;
use risc0_zkvm::guest::env;

fn main() {
    // This is intentionally a "passthrough" logic circuit for testing composition + app_data
    // binding. It proves that some LogicInstance was provided and commits to it.
    // Uses risc0 serde (word-aligned) for compatibility with batch aggregation verification.
    let instance: LogicInstance = env::read();
    env::commit(&instance);
}

