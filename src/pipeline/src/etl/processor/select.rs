// Copyright 2023 Greptime Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use ahash::{HashSet, HashSetExt};
use snafu::OptionExt;
use vrl::value::{KeyString, Value as VrlValue};

use crate::error::{
    Error, KeyMustBeStringSnafu, ProcessorUnsupportedValueSnafu, Result, ValueMustBeMapSnafu,
};
use crate::etl::field::Fields;
use crate::etl::processor::{
    yaml_new_field, yaml_new_fields, yaml_string, FIELDS_NAME, FIELD_NAME, TYPE_NAME,
};
use crate::Processor;

pub(crate) const PROCESSOR_SELECT: &str = "select";
const INCLUDE_KEY: &str = "include";
const EXCLUDE_KEY: &str = "exclude";

#[derive(Debug, Default)]
pub enum SelectType {
    #[default]
    Include,
    Exclude,
}

impl TryFrom<String> for SelectType {
    type Error = Error;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        match value.as_str() {
            INCLUDE_KEY => Ok(SelectType::Include),
            EXCLUDE_KEY => Ok(SelectType::Exclude),
            _ => ProcessorUnsupportedValueSnafu {
                processor: PROCESSOR_SELECT.to_string(),
                val: format!("'{}', expect '{}' or '{}'", value, INCLUDE_KEY, EXCLUDE_KEY),
            }
            .fail(),
        }
    }
}

#[derive(Debug, Default)]
pub struct SelectProcessor {
    fields: Fields,
    select_type: SelectType,
}

impl TryFrom<&yaml_rust::yaml::Hash> for SelectProcessor {
    type Error = Error;

    fn try_from(value: &yaml_rust::yaml::Hash) -> std::result::Result<Self, Self::Error> {
        let mut fields = Fields::default();
        let mut select_type = SelectType::default();

        for (k, v) in value.iter() {
            let key = k
                .as_str()
                .with_context(|| KeyMustBeStringSnafu { k: k.clone() })?;
            match key {
                FIELD_NAME => {
                    fields = Fields::one(yaml_new_field(v, FIELD_NAME)?);
                }
                FIELDS_NAME => {
                    fields = yaml_new_fields(v, FIELDS_NAME)?;
                }
                TYPE_NAME => {
                    select_type = SelectType::try_from(yaml_string(v, TYPE_NAME)?)?;
                }
                _ => {}
            }
        }

        Ok(SelectProcessor {
            fields,
            select_type,
        })
    }
}

impl Processor for SelectProcessor {
    fn kind(&self) -> &str {
        PROCESSOR_SELECT
    }

    fn ignore_missing(&self) -> bool {
        true
    }

    fn exec_mut(&self, mut val: VrlValue) -> Result<VrlValue> {
        let v_map = val.as_object_mut().context(ValueMustBeMapSnafu)?;

        match self.select_type {
            SelectType::Include => {
                let mut include_key_set = HashSet::with_capacity(v_map.len());
                for field in self.fields.iter() {
                    // If the field has a target, move the value to the target
                    let field_name = field.input_field();
                    if let Some(target_name) = field.target_field() {
                        if let Some(v) = v_map.remove(field_name) {
                            v_map.insert(KeyString::from(target_name), v);
                        }
                        include_key_set.insert(target_name);
                    } else {
                        include_key_set.insert(field_name);
                    }
                }
                v_map.retain(|k, _| include_key_set.contains(k.as_str()));
            }
            SelectType::Exclude => {
                for field in self.fields.iter() {
                    v_map.remove(field.input_field());
                }
            }
        }

        Ok(val)
    }
}

#[cfg(test)]
mod test {
    use std::collections::BTreeMap;

    use vrl::prelude::Bytes;
    use vrl::value::{KeyString, Value as VrlValue};

    use crate::etl::field::{Field, Fields};
    use crate::etl::processor::select::{SelectProcessor, SelectType};
    use crate::Processor;

    #[test]
    fn test_select() {
        let processor = SelectProcessor {
            fields: Fields::one(Field::new("hello", None)),
            select_type: SelectType::Include,
        };

        let mut p = BTreeMap::new();
        p.insert(
            KeyString::from("hello"),
            VrlValue::Bytes(Bytes::from("world".to_string())),
        );
        p.insert(
            KeyString::from("hello2"),
            VrlValue::Bytes(Bytes::from("world2".to_string())),
        );

        let result = processor.exec_mut(VrlValue::Object(p));
        assert!(result.is_ok());
        let mut result = result.unwrap();
        let p = result.as_object_mut().unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(
            p.get(&KeyString::from("hello")),
            Some(&VrlValue::Bytes(Bytes::from("world".to_string())))
        );
    }

    #[test]
    fn test_select_with_target() {
        let processor = SelectProcessor {
            fields: Fields::one(Field::new("hello", Some("hello3".to_string()))),
            select_type: SelectType::Include,
        };

        let mut p = BTreeMap::new();
        p.insert(
            KeyString::from("hello"),
            VrlValue::Bytes(Bytes::from("world".to_string())),
        );
        p.insert(
            KeyString::from("hello2"),
            VrlValue::Bytes(Bytes::from("world2".to_string())),
        );

        let result = processor.exec_mut(VrlValue::Object(p));
        assert!(result.is_ok());
        let mut result = result.unwrap();
        let p = result.as_object_mut().unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(
            p.get(&KeyString::from("hello3")),
            Some(&VrlValue::Bytes(Bytes::from("world".to_string())))
        );
    }

    #[test]
    fn test_select_with_exclude() {
        let processor = SelectProcessor {
            fields: Fields::one(Field::new("hello", None)),
            select_type: SelectType::Exclude,
        };

        let mut p = BTreeMap::new();
        p.insert(
            KeyString::from("hello"),
            VrlValue::Bytes(Bytes::from("world".to_string())),
        );
        p.insert(
            KeyString::from("hello2"),
            VrlValue::Bytes(Bytes::from("world2".to_string())),
        );

        let result = processor.exec_mut(VrlValue::Object(p));
        assert!(result.is_ok());
        let mut result = result.unwrap();
        let p = result.as_object_mut().unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p.get(&KeyString::from("hello")), None);
        assert_eq!(
            p.get(&KeyString::from("hello2")),
            Some(&VrlValue::Bytes(Bytes::from("world2".to_string())))
        );
    }
}
