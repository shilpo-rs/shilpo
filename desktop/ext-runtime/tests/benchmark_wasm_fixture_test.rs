#![cfg(feature = "sdk-fixture")]

use std::sync::Arc;

use shilpo_ext_api::ExtensionId;
use shilpo_ext_runtime::{
    ExtensionRuntime, FakeSecretBroker, RuntimeBudget, WasmModule, WasmRuntime,
};

const SDK_FIXTURE: &[u8] = include_bytes!(env!("SHILPO_SDK_FIXTURE_WASM"));

#[test]
fn test_benchmark_wasm_fixture_availability_and_cold_load() {
    assert!(
        !SDK_FIXTURE.is_empty(),
        "SHILPO_SDK_FIXTURE_WASM fixture bytes must not be empty"
    );

    let broker = Arc::new(FakeSecretBroker::new());
    let mut runtime =
        WasmRuntime::with_broker(broker).expect("create WasmRuntime with fake broker");
    let module = WasmModule::from_bytes(SDK_FIXTURE);
    let budget = RuntimeBudget::default();

    let ext_id = ExtensionId::new("io.shilpo.benchmark-fixture-test").unwrap();

    // 1. Load the SDK component
    let load_res = runtime.load(&ext_id, module, budget);
    assert!(
        load_res.is_ok(),
        "SDK component fixture must load cleanly: {load_res:?}"
    );

    // 2. Unload the SDK component
    let unload_res = runtime.unload(&ext_id);
    assert!(
        unload_res.is_ok(),
        "SDK component fixture must unload cleanly: {unload_res:?}"
    );
}
