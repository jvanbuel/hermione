//! Generated gRPC/protobuf types for Hermione.
//!
//! The protobuf schema lives in `proto/hermione.proto` and is compiled by
//! `build.rs` at build time.

pub mod v1 {
    tonic::include_proto!("hermione.v1");
}

pub use v1::*;
