
# Celenium (CEN)

Celenium is a separate proof-of-work blockDAG network using GHOSTDAG and kHeavyHash, developed from Kaspa's Rust codebase. This repository contains the Celenium full node, wallet libraries, RPC services, and Stratum bridge.

The work in this fork covers Celenium's network identity, emission schedule, premine vesting, mining connectivity, and participation payouts. The code and verification notes describe these changes separately from the protocol inherited from upstream.

## Relationship to Kaspa

Celenium is a fork of [kaspanet/rusty-kaspa](https://github.com/kaspanet/rusty-kaspa), the Rust implementation of the Kaspa full node. The blockDAG foundation and substantial parts of this codebase originate with the Kaspa developers.

- **Upstream project:** [Kaspa on GitHub](https://github.com/kaspanet).
- **Upstream contributors:** [`michaelsutton`](https://github.com/michaelsutton), [`someone235`](https://github.com/someone235), [`biryukovmaxim`](https://github.com/biryukovmaxim), [`aspect`](https://github.com/aspect), and [`tiram88`](https://github.com/tiram88) appear in the [upstream contributor history](https://github.com/kaspanet/rusty-kaspa/graphs/contributors). This is credit for upstream work, not a list of Celenium team members.
- **License:** The original Kaspa copyright and [ISC license](LICENSE) are retained.

Celenium is maintained independently. Upstream attribution does not imply that Kaspa developers maintain, endorse, or participate in the Celenium project. CEN uses a separate chain and address namespace; it is not KAS.

## What this fork changes

- **Network identity:** Celenium genesis parameters, network ports, and `cen69` address prefixes. See [network parameters](docs/network-params.md).
- **Emission and allocation:** A Celenium-specific emission schedule and premine outputs protected by time locks and multisignature conditions. See [tokenomics](docs/tokenomics.md), [emission schedule](docs/emission-schedule.md), and [premine vesting](docs/premine-cltv-schedule.md).
- **Participation claims:** A bridge-managed daily reward pool with funding checks, signed wallet transactions, and persistent payout recovery. See [claim accounting](docs/faucet-drip.md) and [direct-claim deployment verification](docs/reservoir-verification/direct-claim-mainnet-deployment.md).
- **Mining connectivity:** Stratum services for external workers and APIs used by the Celenium Miner companion application. See [bridge documentation](bridge/docs/README.md).

## Status and operational limits

The direct-claim bridge release is deployed on the Celenium mainnet service. Deployment does not establish that a wallet has received a payment: claims require verified funds, a valid transaction, and blockchain confirmation. Mining shares and off-chain participation balances are not confirmed wallet balances.

The deployment verification records an unfunded reservoir and a node still syncing at the time of verification. Read current node and bridge responses before making operational decisions. General worker PPLNS settlement is not established by the direct-claim tests.

This repository's development branch is `main`. The [Crescendo guide](docs/crescendo-guide.md) is inherited protocol background; its Kaspa activation history is not a Celenium launch event.

## Maintainers and contributions

Celenium development is maintained by [Dimas A. Pradana (`dimas405`)](https://github.com/dimas405). Commit history records the authors of the work retained in this repository; upstream contributors are acknowledged separately above.

Read [CONTRIBUTING.md](CONTRIBUTING.md) before proposing a change. Include a reproducible problem, the affected network, and test evidence. Keep private keys, recovery phrases, signing credentials, and access tokens out of commits and issue reports.



## Installation
  <details>
  <summary>Building on Linux</summary>

  1. Install general prerequisites

      ```bash
      sudo apt install curl git build-essential libssl-dev pkg-config
      ```

  2. Install Protobuf (required for gRPC)

      ```bash
      sudo apt install protobuf-compiler libprotobuf-dev #Required for gRPC
      ```
  3. Install the clang toolchain (required for RocksDB and WASM secp256k1 builds)

      ```bash
      sudo apt-get install clang-format clang-tidy \
      clang-tools clang clangd libc++-dev \
      libc++1 libc++abi-dev libc++abi1 \
      libclang-dev libclang1 liblldb-dev \
      libllvm-ocaml-dev libomp-dev libomp5 \
      lld lldb llvm-dev llvm-runtime \
      llvm python3-clang
      ```
  4. Install the [rust toolchain](https://rustup.rs/)

     If you already have rust installed, update it by running: `rustup update`
  5. Install wasm-pack
      ```bash
      cargo install wasm-pack
      ```
  6. Install wasm32 target
      ```bash
      rustup target add wasm32-unknown-unknown
      ```
  7. Clone the repo
      ```bash
      git clone https://github.com/celeniumcore/celenium.git
      cd celenium
      ```
  </details>



  <details>
  <summary>Building on Windows</summary>


  1. [Install Git for Windows](https://gitforwindows.org/) or an alternative Git distribution.

  2. Install [Protocol Buffers](https://github.com/protocolbuffers/protobuf/releases/download/v21.10/protoc-21.10-win64.zip) and add the `bin` directory to your `Path`


3. Install [LLVM-15.0.6-win64.exe](https://github.com/llvm/llvm-project/releases/download/llvmorg-15.0.6/LLVM-15.0.6-win64.exe)

    Add the `bin` directory of the LLVM installation (`C:\Program Files\LLVM\bin`) to PATH

    set `LIBCLANG_PATH` environment variable to point to the `bin` directory as well

    **IMPORTANT:** Due to C++ dependency configuration issues, LLVM `AR` installation on Windows may not function correctly when switching between WASM and native C++ code compilation (native `RocksDB+secp256k1` vs WASM32 builds of `secp256k1`). Unfortunately, manually setting `AR` environment variable also confuses C++ build toolchain (it should not be set for native but should be set for WASM32 targets). Currently, the best way to address this, is as follows: after installing LLVM on Windows, go to the target `bin` installation directory and copy or rename `LLVM_AR.exe` to `AR.exe`.

  4. Install the [rust toolchain](https://rustup.rs/)

     If you already have rust installed, update it by running: `rustup update`
  5. Install wasm-pack
      ```bash
      cargo install wasm-pack
      ```
  6. Install wasm32 target
      ```bash
      rustup target add wasm32-unknown-unknown
      ```
  7. Clone the repo
      ```bash
      git clone https://github.com/celeniumcore/celenium.git
      cd celenium
      ```
 </details>


  <details>
  <summary>Building on Mac OS</summary>


  1. Install Protobuf (required for gRPC)
      ```bash
      brew install protobuf
      ```
  2. Install llvm.

      The default XCode installation of `llvm` does not support WASM build targets.
To build WASM on MacOS you need to install `llvm` from homebrew (at the time of writing, the llvm version for MacOS is 16.0.1).
      ```bash
      brew install llvm
      ```

      **NOTE:** Homebrew can use different keg installation locations depending on your configuration. For example:
      - `/opt/homebrew/opt/llvm` -> `/opt/homebrew/Cellar/llvm/16.0.1`
      - `/usr/local/Cellar/llvm/16.0.1`

      To determine the installation location you can use `brew list llvm` command and then modify the paths below accordingly:
      ```bash
      % brew list llvm
      /usr/local/Cellar/llvm/16.0.1/bin/FileCheck
      /usr/local/Cellar/llvm/16.0.1/bin/UnicodeNameMappingGenerator
      ...
      ```
      If you have `/opt/homebrew/Cellar`, then you should be able to use `/opt/homebrew/opt/llvm`.

      Add the following to your `~/.zshrc` file:
      ```bash
      export PATH="/opt/homebrew/opt/llvm/bin:$PATH"
      export LDFLAGS="-L/opt/homebrew/opt/llvm/lib"
      export CPPFLAGS="-I/opt/homebrew/opt/llvm/include"
      export AR=/opt/homebrew/opt/llvm/bin/llvm-ar
      ```

      Reload the `~/.zshrc` file
      ```bash
      source ~/.zshrc
      ```
  3. Install the [rust toolchain](https://rustup.rs/)

     If you already have rust installed, update it by running: `rustup update`
  4. Install wasm-pack
      ```bash
      cargo install wasm-pack
      ```
  5. Install wasm32 target
      ```bash
      rustup target add wasm32-unknown-unknown
      ```
  6. Clone the repo
      ```bash
      git clone https://github.com/celeniumcore/celenium.git
      cd celenium
      ```

 </details>

 <details>
 <summary> Building with Docker </summary>

  You can build the project using Docker in two ways: a simple single-architecture build, or a multi-architecture build using the provided script.

  #### 1. Simple Docker Build

  To build for your current architecture (e.g., `linux/amd64`):

  ```sh
  docker build -f docker/Dockerfile.celeniumd -t celeniumd:latest .
  ```

  Replace `Dockerfile.celeniumd` with the appropriate Dockerfile for your target (`celeniumd`, `celenium-wallet`, `pradana`, or `simpa`).

  #### 2. Multi-Architecture Build

  To build images for multiple architectures (e.g., `linux/amd64` and `linux/arm64`) and optionally push them to a registry, use the `build-docker-multi-arch.sh` script:

  ```sh
  ./build-docker-multi-arch.sh --tag <tag> --artifact <artifact> [--arches "<arches>"] [--push]
  ```

  - `--tag <tag>`: **(required)** The Docker image tag to use.
  - `--artifact <artifact>`: The build target/artifact (default: `celeniumd`). Must match the Dockerfile name, e.g., `celeniumd` for `Dockerfile.celeniumd`.
  - `--arches "<arches>"`: Space-separated list of architectures (default: `"linux/amd64 linux/arm64"`).
  - `--push`: If specified, the built images will be pushed to your Docker registry.

  **Examples:**

  Build and push a multi-arch image for `celeniumd`:

  ```sh
  ./build-docker-multi-arch.sh --tag myrepo/celeniumd:latest --artifact celeniumd --push
  ```

  Build a multi-arch image for `celenium-wallet` without pushing:

  ```sh
  ./build-docker-multi-arch.sh --tag celenium-wallet:test --artifact celenium-wallet
  ```

  **Note:**  
  In order to use `build-docker-multi-arch.sh`, you need Docker with Buildx enabled.
 </details>

  <details>

  <summary>Building WASM32 SDK</summary>

  Rust WebAssembly (WASM) refers to the use of the Rust programming language to write code that can be compiled into WebAssembly, a binary instruction format that runs in web browsers and NodeJs. This allows for easy development using JavaScript and TypeScript programming languages while retaining the benefits of Rust.

  WASM SDK components can be built from sources by running:
    - `./build-release` - build a full release package (includes both release and debug builds for web and nodejs targets)
    - `./build-docs` - build TypeScript documentation
    - `./build-web` - release web build
    - `./build-web-dev` - development web build
    - `./build-nodejs` - release nodejs build
    - `./build-nodejs-dev` - development nodejs build

  IMPORTANT: do not use `dev` builds in production. They are significantly larger, slower and include debug symbols.

### Requirements

  - NodeJs (v20+): https://nodejs.org/en
  - TypeDoc: https://typedoc.org/

### Builds & documentation

  - Release builds: https://github.com/celeniumcore/celenium/releases
  - Developer builds: build from source (see above)
  - TypeScript documentation: generate via `./build-docs` in `wasm/`

  </details>
<details>

<summary>
Celenium CLI + Wallet
</summary>

`celenium-cli` crate provides a cli-driven RPC interface to the node and a
terminal interface to the Rusty Celenium Wallet runtime. These wallets are
compatible with WASM SDK Wallet API and Celenium NG projects.


```bash
cd cli
cargo run --release
```

</details>



<details>

<summary>
Local Web Wallet
</summary>

Run an http server inside of `wallet/wasm/web` folder. If you don't have once, you can use the following:

```bash
cd wallet/wasm/web
cargo install basic-http-server
basic-http-server
```
The *basic-http-server* will serve on port 4000 by default, so open your web browser and load http://localhost:4000

The framework is compatible with all major desktop and mobile browsers.


</details>


## Running the node

  **Start a mainnet node**

  ```bash
  cargo run --release --bin celeniumd
  # or with UTXO-index enabled (needed when using wallets)
  cargo run --release --bin celeniumd -- --utxoindex
  ```
  **Start a testnet node**

  ```bash
cargo run --release --bin celeniumd -- --testnet
  ```

<details>
  <summary>
    Start a devnet node
  </summary>

Start the DevNet node with the following command:

```bash
cargo run --bin celeniumd -- --devnet --enable-unsynced-mining --rpclisten=127.0.0.1 --rpclisten-borsh=127.0.0.1 --utxoindex
```
* `enable-unsynced-mining` is required when the network isn't synchronized, which is the case on the first launch
* `utxoindex` enables the UTXO index, which is necessary for wallet functionality.
* `rpclisten` and `rpclisten-borsh` are likely to be required by mining software

note: it will take a bit of time for difficulty to adjust, so you may need to wait a bit before you see blocks being mined consistently.

</details>

<details>

  <summary>
Using a configuration file
  </summary>

  ```bash
cargo run --release --bin celeniumd -- --configfile /path/to/configfile.toml
# or
cargo run --release --bin celeniumd -- -C /path/to/configfile.toml
  ```
  - The config file should be a list of \<CLI argument\> = \<value\> separated by newlines.
  - Whitespace around the `=` is fine, `arg=value` and `arg = value` are both parsed correctly.
  - Values with special characters like `.` or `=` will require quoting the value i.e \<CLI argument\> = "\<value\>".
  - Arguments with multiple values should be surrounded with brackets like `addpeer = ["10.0.0.1", "1.2.3.4"]`.

  For example:
  ```
testnet = true
utxoindex = false
disable-upnp = true
perf-metrics = true
appdir = "some-dir"
netsuffix = 11
addpeer = ["10.0.0.1", "1.2.3.4"]
  ```
 Pass the `--help` flag to view all possible arguments

  ```bash
cargo run --release --bin celeniumd -- --help
  ```
</details>

<details>

  <summary>
wRPC
  </summary>

  wRPC subsystem is disabled by default in `celeniumd` and can be enabled via:


  JSON protocol:
  ```bash
  --rpclisten-json=<interface:port>
  # or use the defaults for current network
  --rpclisten-json=default
  ```

  Borsh protocol:
  ```bash
  --rpclisten-borsh=<interface:port>
  # or use the defaults for current network
  --rpclisten-borsh=default
  ```

  **Sidenote:**

  Rusty Celenium integrates an optional wRPC
  subsystem. wRPC is a high-performance, platform-neutral, Rust-centric, WebSocket-framed RPC
  implementation that can use [Borsh](https://borsh.io/) and JSON protocol encoding.

  JSON protocol messaging
  is similar to JSON-RPC 1.0, but differs from the specification due to server-side
  notifications.

  [Borsh](https://borsh.io/) encoding is meant for inter-process communication. When using [Borsh](https://borsh.io/)
  both client and server should be built from the same codebase.

  JSON protocol is based on
  Celenium data structures and is data-structure-version agnostic. You can connect to the
  JSON endpoint using any WebSocket library. Built-in RPC clients for JavaScript and
  TypeScript capable of running in web browsers and Node.js are available as a part of
  the Celenium WASM framework.

</details>

## Stratum Bridge Beta

The bridge is developed as part of Celenium. For a bug report, include the affected network, bridge version, relevant logs with secrets removed, and a reproducible example.

Check out the [README.md](bridge/docs/README.md) for instructions on how to run the stratum bridge.

## Benchmarking & Testing


<details>

<summary>Simulation framework (Simpa)</summary>

The current codebase supports a full in-process network simulation, building an actual DAG over virtual time with virtual delay and benchmarking validation time (following the simulation generation).

To see the available commands
```bash
cargo run --release --bin simpa -- --help
```

The following command will run a simulation to produce 1000 blocks with communication delay of 2 seconds and 8 BPS (blocks per second) while attempting to fill each block with up to 200 transactions.

```bash
cargo run --release --bin simpa -- -t=200 -d=2 -b=8 -n=1000
```

</details>




<details>

<summary>Heap Profiling</summary>

Heap-profiling in `celeniumd` and `simpa` can be done by enabling `heap` feature and profile using the `--features` argument

```bash
cargo run --bin celeniumd --profile heap --features=heap
```

It will produce `{bin-name}-heap.json` file in the root of the workdir, that can be inspected by the [dhat-viewer](https://github.com/unofficial-mirror/valgrind/tree/master/dhat)

</details>


<details>

<summary>Tests</summary>


**Run unit and most integration tests**

```bash
cd celenium
cargo test --release
// or install nextest and run
```



**Using nextest**

```bash
cd celenium
cargo nextest run --release
```

</details>

<details>

<summary>Lints</summary>

```bash
cd celenium
./check
```

</details>


<details>

<summary>Benchmarks</summary>

```bash
cd celenium
cargo bench
```

</details>

<details>

<summary>Logging</summary>

Logging in `celeniumd` and `simpa` can be [filtered](https://docs.rs/env_logger/0.10.0/env_logger/#filtering-results) by either:

1. Defining the environment variable `RUST_LOG`
2. Adding the --loglevel argument like in the following example:

    ```
    (cargo run --bin celeniumd -- --loglevel info,celenium_rpc_core=trace,celenium_grpc_core=trace,consensus=trace,celenium_core=trace) 2>&1 | tee ~/celenium.log
    ```
    In this command we set the `loglevel` to `INFO`.

</details>

<details>

<summary>Override consensus parameters</summary>

You can experiment with non-standard consensus parameters in non-mainnet environments by supplying a JSON file with `--override-params-file <path>`. See [docs/override-params.md](docs/override-params.md) for a more detailed explanation.

</details>
