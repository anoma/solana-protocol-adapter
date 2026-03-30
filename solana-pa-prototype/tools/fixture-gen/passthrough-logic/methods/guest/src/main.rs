use arm::logic_instance::LogicInstance;
use risc0_zkvm::guest::env;

fn main() {
    let mut instance: LogicInstance = env::read();
    instance.compute_and_set_app_data_hash();
    env::commit(&instance);
}

