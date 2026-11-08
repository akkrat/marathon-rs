//! Port of core/src/main/kotlin/com/malinskiy/marathon/execution/strategy/impl/pooling/OmniPoolingStrategy.kt
//! and impl/pooling/parameterized/{Abi,Manufacturer,Model,OperatingSystemVersion,Combo}PoolingStrategy.kt.

use crate::{model::DevicePoolId, vendor::Device};

use marathon_config::schema::PoolingStrategyConfiguration;

#[derive(Clone, Debug)]
pub enum PoolingStrategy {
    Omni,
    Abi,
    Manufacturer,
    Model,
    OperatingSystemVersion,
    Combo(Vec<PoolingStrategy>),
}

impl PoolingStrategy {
    pub fn from_config(cnf: &PoolingStrategyConfiguration) -> Self {
        match cnf {
            PoolingStrategyConfiguration::Omni => Self::Omni,
            PoolingStrategyConfiguration::Abi => Self::Abi,
            PoolingStrategyConfiguration::Manufacturer => Self::Manufacturer,
            PoolingStrategyConfiguration::Model => Self::Model,
            PoolingStrategyConfiguration::OperatingSystemVersion => Self::OperatingSystemVersion,
            PoolingStrategyConfiguration::Combo(list) => Self::Combo(list.iter().map(Self::from_config).collect()),
        }
    }

    pub fn associate(&self, device: &dyn Device) -> DevicePoolId {
        let name = match self {
            Self::Omni => "omni".into(),
            Self::Abi => device.abi(),
            Self::Manufacturer => device.info().manufacturer,
            Self::Model => device.info().model,
            Self::OperatingSystemVersion => device.info().operating_system.version,
            Self::Combo(list) => {
                // The separator is appended only after a non-empty prefix, so
                // leading empty names vanish while inner ones double the `_`.
                let mut acc = String::default();
                for strategy in list {
                    if !acc.is_empty() {
                        acc.push('_');
                    }
                    acc.push_str(&strategy.associate(device).name);
                }
                acc
            }
        };
        DevicePoolId { name }
    }
}
