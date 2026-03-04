use arm::logic_instance::LogicInstance;
use risc0_zkvm::guest::env;

fn main() {
    // Minimal circuit: tests composition and app_data binding without real logic verification.
    let instance: LogicInstance = env::read();
    env::commit(&instance);
}

