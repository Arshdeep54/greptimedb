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

#![feature(let_chains)]
#![feature(try_blocks)]

mod admin;
mod flush_flow;
mod macros;
mod system;

pub mod aggrs;
pub mod function;
pub mod function_factory;
pub mod function_registry;
pub mod handlers;
pub mod helper;
pub mod scalars;
pub mod state;
pub mod utils;
