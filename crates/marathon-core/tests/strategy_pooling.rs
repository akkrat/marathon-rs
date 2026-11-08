//! Port of core/src/test/kotlin/com/malinskiy/marathon/execution/strategy/impl/pooling/OmniPoolingStrategyTest.kt
//! and impl/pooling/parameterized/{Abi,Combo,Manufacturer,OperatingSystemVersion}PoolingStrategyTest.kt,
//! ModelPoolingStrategySpek.kt (class ModelPoolingStrategyTest).

mod strategy_support;

use marathon_config::schema::PoolingStrategyConfiguration as Cnf;
use marathon_core::{
    model::{DevicePoolId, NetworkState},
    strategy::PoolingStrategy,
};
use strategy_support::DeviceStub;

fn strategy(cnf: Cnf) -> PoolingStrategy {
    PoolingStrategy::from_config(&cnf)
}
fn name(strategy: &PoolingStrategy, device: &DeviceStub) -> String {
    strategy.associate(device).name.to_owned()
}

#[test]
fn should_return_same_device_pool_id_for_all_devices() {
    let strategy = strategy(Cnf::Omni);
    let device1 = DeviceStub::new()
        .operating_system("22")
        .serial("Serial", NetworkState::Connected, true);
    let device2 = DeviceStub::new()
        .operating_system("25")
        .serial("Serial2", NetworkState::Disconnected, false);
    let omni = DevicePoolId { name: "omni".into() };
    assert_eq!(strategy.associate(&device1), omni);
    assert_eq!(strategy.associate(&device2), omni);
}

#[test]
fn should_return_device_pool_id_with_name_equals_to_device_abi() {
    let device = DeviceStub::new().abi("Test_ABI");
    assert_eq!(name(&strategy(Cnf::Abi), &device), "Test_ABI");
}

#[test]
fn should_return_device_pool_id_with_name_equals_to_device_manufacturer() {
    let device = DeviceStub::new().manufacturer("TestDeviceManufacturer");
    assert_eq!(name(&strategy(Cnf::Manufacturer), &device), "TestDeviceManufacturer");
}

#[test]
fn should_return_device_pool_id_with_name_equals_to_device_model() {
    let device = DeviceStub::new().model("TestModel");
    assert_eq!(name(&strategy(Cnf::Model), &device), "TestModel");
}

#[test]
fn should_return_device_pool_id_with_name_equals_to_device_operating_system_version() {
    let device = DeviceStub::new().operating_system("27");
    assert_eq!(name(&strategy(Cnf::OperatingSystemVersion), &device), "27");
}

#[test]
fn one_strategy_should_return_device_pool_id_name_equals_to_device_pool_id_name_from_base_strategy() {
    let combo = strategy(Cnf::Combo(vec![Cnf::Abi]));
    assert_eq!(name(&combo, &DeviceStub::new().abi("x64")), "x64");
}

#[test]
fn two_strategies_should_return_device_pool_id_name_equals_device_pool_id0_name_device_pool_id1_name() {
    let combo = strategy(Cnf::Combo(vec![Cnf::Model, Cnf::Abi]));
    let device = DeviceStub::new().abi("x64").model("TestDeviceModel");
    assert_eq!(name(&combo, &device), "TestDeviceModel_x64");
}

#[test]
fn three_strategies_should_return_device_pool_id_name_equals_device_pool_id0_name_device_pool_id1_name_device_pool_id2_name()
 {
    let combo = strategy(Cnf::Combo(vec![Cnf::Manufacturer, Cnf::Model, Cnf::Abi]));
    let device = DeviceStub::new()
        .abi("x64")
        .model("TestDeviceModel")
        .manufacturer("TestDeviceManufacturer");
    assert_eq!(name(&combo, &device), "TestDeviceManufacturer_TestDeviceModel_x64");
}

#[test]
fn combo_skips_the_separator_after_leading_empty_names() {
    let combo = strategy(Cnf::Combo(vec![Cnf::Abi, Cnf::Model]));
    assert_eq!(name(&combo, &DeviceStub::new().abi("").model("x")), "x");
    let combo = strategy(Cnf::Combo(vec![Cnf::Manufacturer, Cnf::Abi, Cnf::Model]));
    let device = DeviceStub::new().manufacturer("a").abi("").model("b");
    assert_eq!(name(&combo, &device), "a__b");
    assert_eq!(name(&strategy(Cnf::Combo(Vec::new())), &device), "");
    let nested = strategy(Cnf::Combo(vec![Cnf::Combo(vec![Cnf::Abi, Cnf::Model]), Cnf::Omni]));
    assert_eq!(name(&nested, &DeviceStub::new().abi("").model("")), "omni");
}
