# select build image
FROM rust:latest as build

# create a new empty shell project
RUN USER=root cargo new --bin suji
WORKDIR /suji

# copy over your manifests
COPY ./Cargo.lock ./Cargo.lock
COPY ./Cargo.toml ./Cargo.toml

# this build step will cache your dependencies
RUN cargo build --release
RUN rm src/*.rs

# copy your source tree
COPY ./src ./src

# build for release mode
RUN rm ./target/release/deps/suji*
RUN cargo build --release

# our final base - use the debug one so we can copy things
FROM gcr.io/distroless/cc-debian12:debug
ARG ARCH=x86_64

# copy the build artifact from the build stage
COPY --from=build /suji/target/release/suji .
COPY --from=build /usr/lib/${ARCH}-linux-gnu/libssl3.so* /usr/lib/${ARCH}-linux-gnu/

CMD ["-c", "/suji $CONFIG_FILE"]