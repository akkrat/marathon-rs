//! Shared implicit-scalar contract for the native differential comparator.

use snakeyaml::resolve::{Implicit, resolve_implicit};

#[test]
fn implicit_scalar_contract() {
    for line in include_str!("../../../fixtures/contract/yaml-scalars.tsv").lines() {
        let (scalar, expected) = line.split_once('\t').expect("scalar and expected type");
        let actual = match resolve_implicit(scalar) {
            Implicit::Bool => "bool",
            Implicit::Int => "int",
            Implicit::Float => "float",
            Implicit::Null => "null",
            Implicit::Str => "str",
        };
        assert_eq!(actual, expected, "{scalar:?}");
    }
}
