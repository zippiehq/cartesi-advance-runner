FROM rust:1.85.1 as base
RUN apt-get update && apt-get install -y protobuf-compiler clang libboost1.81-dev git lua5.4

WORKDIR /advance-runner
COPY src /advance-runner/src
COPY Cargo.toml /advance-runner/Cargo.toml
COPY Cargo.lock /advance-runner/Cargo.lock
RUN git config --global url."https://github.com/".insteadOf git@github.com: 
RUN CARGO_NET_GIT_FETCH_WITH_CLI=true cargo build --release

