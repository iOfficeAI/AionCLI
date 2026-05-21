# Changelog

## [0.1.8](https://github.com/iOfficeAI/AionCore/compare/v0.1.7...v0.1.8) (2026-05-21)


### Features

* add is_full_url flag for provider URL resolution ([#307](https://github.com/iOfficeAI/AionCore/issues/307)) ([3aa15da](https://github.com/iOfficeAI/AionCore/commit/3aa15da0c70a15da097e5bd839b83c4c0c720bf1))


### Bug Fixes

* **ai-agent:** prevent stuck session after ACP cancel ([#313](https://github.com/iOfficeAI/AionCore/issues/313)) ([3a84bfe](https://github.com/iOfficeAI/AionCore/commit/3a84bfec1bfffd589d091efdd7b157ea1c3b2960))
* **runtime:** create node symlink in bundled bun directory (ELECTRON-1EY) ([#310](https://github.com/iOfficeAI/AionCore/issues/310)) ([c0ad26b](https://github.com/iOfficeAI/AionCore/commit/c0ad26bb74008609a8dac815758aabc2284a8066))

## [0.1.7](https://github.com/iOfficeAI/AionCore/compare/v0.1.6...v0.1.7) (2026-05-19)


### Bug Fixes

* **ai-agent:** surface ACP startup crashes and accept work_dir paths (ELECTRON-1BT) ([#305](https://github.com/iOfficeAI/AionCore/issues/305)) ([7aa29a7](https://github.com/iOfficeAI/AionCore/commit/7aa29a78a2fa5013b9a4845217ba89d4b045822b))

## [0.1.6](https://github.com/iOfficeAI/AionCore/compare/v0.1.5...v0.1.6) (2026-05-19)


### Bug Fixes

* **ai-agent:** force-kill ACP processes on Windows (ELECTRON-1E9) ([#303](https://github.com/iOfficeAI/AionCore/issues/303)) ([e60fdd3](https://github.com/iOfficeAI/AionCore/commit/e60fdd31332512398715ed056a7f60eeee42a752))
* **ai-agent:** make find_native_claude cross-platform (ELECTRON-1CG) ([#299](https://github.com/iOfficeAI/AionCore/issues/299)) ([fda9239](https://github.com/iOfficeAI/AionCore/commit/fda92398caa9384d8f0cdc11cf0a3616047448af))
* **ai-agent:** return 409 when remote WS not connected on cancel (ELECTRON-1CV) ([#302](https://github.com/iOfficeAI/AionCore/issues/302)) ([dc87f1c](https://github.com/iOfficeAI/AionCore/commit/dc87f1c37352be6cd820503ed4c38be4098d26ed))


### Documentation

* catch up with aionui-backend → AionCore rename ([#301](https://github.com/iOfficeAI/AionCore/issues/301)) ([40a7e83](https://github.com/iOfficeAI/AionCore/commit/40a7e83618bb62b145378e333e26b66dc0061c89))

## [0.1.5](https://github.com/iOfficeAI/AionCore/compare/v0.1.4...v0.1.5) (2026-05-19)


### Features

* **ai-agent:** add cc-switch provider env injection for Claude ACP ([#291](https://github.com/iOfficeAI/AionCore/issues/291)) ([a7b93e7](https://github.com/iOfficeAI/AionCore/commit/a7b93e7dde78a7b254e26e2d2e25d7b9b885ad5b))


### Bug Fixes

* **channel:** pass model via extra for non-aionrs conversations ([#298](https://github.com/iOfficeAI/AionCore/issues/298)) ([eb65dfe](https://github.com/iOfficeAI/AionCore/commit/eb65dfed2a9f2ea3d9cb11699c276ba76690c03e))


### Code Refactoring

* rename binary from aioncli to aioncore ([#293](https://github.com/iOfficeAI/AionCore/issues/293)) ([ae78cd1](https://github.com/iOfficeAI/AionCore/commit/ae78cd19f599fb3c8845ba5d3e208a75bf310368))

## [0.1.4](https://github.com/iOfficeAI/AionCLI/compare/v0.1.3...v0.1.4) (2026-05-16)


### Features

* **ai-agent:** log every CLI detection + add doctor subcommand ([#285](https://github.com/iOfficeAI/AionCLI/issues/285)) ([5ef6d0a](https://github.com/iOfficeAI/AionCLI/commit/5ef6d0a4d99345a502a9073dfdfa0d07cfa52a8c))
* **runtime:** full shell-style command in spawn logs ([#278](https://github.com/iOfficeAI/AionCLI/issues/278)) ([dd51616](https://github.com/iOfficeAI/AionCLI/commit/dd516165ae9e22fcb0573ae9d8d3aa094e54cff2))


### Bug Fixes

* **ai-agent:** negotiate OpenClaw protocol v3..v4 ([#288](https://github.com/iOfficeAI/AionCLI/issues/288)) ([dfeece0](https://github.com/iOfficeAI/AionCLI/commit/dfeece0e6a465093090c0efdfa1f5aa93d9fa6e8))
* **team:** model routing + schema unification + lazy warm mode persistence ([#286](https://github.com/iOfficeAI/AionCLI/issues/286)) ([199a392](https://github.com/iOfficeAI/AionCLI/commit/199a392caca600ef215bb2ae71bfd82bda7bb744))


### Performance Improvements

* **team:** lazy warm — only start agent processes on first message ([#282](https://github.com/iOfficeAI/AionCLI/issues/282)) ([6281f31](https://github.com/iOfficeAI/AionCLI/commit/6281f31ac6a2656c1af51891589770f4583e00c2))


### Code Refactoring

* **app:** extract CLI definitions to cli.rs ([#280](https://github.com/iOfficeAI/AionCLI/issues/280)) ([5685d52](https://github.com/iOfficeAI/AionCLI/commit/5685d5237b8f51c70e80895b1c654325c958196e))
* **app:** introduce commands/ module with layered bootstrap for subcommands ([#283](https://github.com/iOfficeAI/AionCLI/issues/283)) ([1216597](https://github.com/iOfficeAI/AionCLI/commit/12165971cfae61d85376c102ef9f9afc5a7c5bbf))
* **app:** replace argv sniffing with clap Subcommand for mcp-* helpers ([#277](https://github.com/iOfficeAI/AionCLI/issues/277)) ([c3d137c](https://github.com/iOfficeAI/AionCLI/commit/c3d137c9e5fdcb12e29d5ca7abd6a0585bbc6c8d))
* **app:** split monolithic lib.rs/main.rs into per-module files ([#284](https://github.com/iOfficeAI/AionCLI/issues/284)) ([f3462cb](https://github.com/iOfficeAI/AionCLI/commit/f3462cbb1d6d830a3a368a76b2d9ea6424f21b64))
* rename binary from aionui-backend to aioncli ([#289](https://github.com/iOfficeAI/AionCLI/issues/289)) ([30eeca3](https://github.com/iOfficeAI/AionCLI/commit/30eeca37661441ba9474aa7dc51ca911abda0bfb))

## [0.1.3](https://github.com/iOfficeAI/aionui-backend/compare/v0.1.2...v0.1.3) (2026-05-15)


### Bug Fixes

* **acp:** apply AvailableCommands event to session aggregate ([#270](https://github.com/iOfficeAI/aionui-backend/issues/270)) ([a46b561](https://github.com/iOfficeAI/aionui-backend/commit/a46b561b20421a59fd73e9629ef452c624781ef2))
* **assistant:** pin user_data_dir to runtime --data-dir ([#274](https://github.com/iOfficeAI/aionui-backend/issues/274)) ([0d49022](https://github.com/iOfficeAI/aionui-backend/commit/0d49022f90d7950e00e0dfdb60e389116177182d))
* **db:** cast REAL timestamps to INTEGER in conversations table ([#275](https://github.com/iOfficeAI/aionui-backend/issues/275)) ([92e5fa9](https://github.com/iOfficeAI/aionui-backend/commit/92e5fa9f75065b85b5533476d0fbb836b0145b4e))
* **runtime:** make CLI detection work on Windows ([#276](https://github.com/iOfficeAI/aionui-backend/issues/276)) ([35bd121](https://github.com/iOfficeAI/aionui-backend/commit/35bd1217425a2e0d51f3e8f8e2f53ea37151c1eb))
* **team:** pass workspace from CreateTeamRequest to agent conversations ([#273](https://github.com/iOfficeAI/aionui-backend/issues/273)) ([f4e3f32](https://github.com/iOfficeAI/aionui-backend/commit/f4e3f32e3a1a9f8fa34769205fa031b6037af00e))

## [0.1.2](https://github.com/iOfficeAI/aionui-backend/compare/v0.1.1...v0.1.2) (2026-05-14)


### Features

* **aionrs:** expose slash commands API ([c9d30ca](https://github.com/iOfficeAI/aionui-backend/commit/c9d30ca63b7840fd997048bb4ffbe1b4976eb63c))
* **aionrs:** expose slash commands via get_slash_commands() ([e6e120a](https://github.com/iOfficeAI/aionui-backend/commit/e6e120a883c522a045360325b325a81033c9d28d))
* **cli:** add --work-dir argument for conversation workspaces ([ed2d394](https://github.com/iOfficeAI/aionui-backend/commit/ed2d3942582245b243d7ab0e25175528a5db7d40))
* **cli:** add --work-dir argument for conversation workspaces ([fdfbbf5](https://github.com/iOfficeAI/aionui-backend/commit/fdfbbf5e36658f6aa4454f3cb5c38332a93f544b))


### Bug Fixes

* **ai-agent:** surface upstream ACP error messages without status prefix ([#268](https://github.com/iOfficeAI/aionui-backend/issues/268)) ([532f7e3](https://github.com/iOfficeAI/aionui-backend/commit/532f7e3bbee7e8389499f4d7bbda198c22363e13))
* **aionrs:** abort engine.run() on cancel ([9eeb0a8](https://github.com/iOfficeAI/aionui-backend/commit/9eeb0a8620d10a3e2de74fa9d37907f3c8ab043a))
* **aionrs:** abort engine.run() on cancel instead of only emitting events ([74024c3](https://github.com/iOfficeAI/aionui-backend/commit/74024c3af6a8277588c4dd28e8453e1822789e15))
* **ci:** allow too_many_arguments on JobExecutor::new ([26918a0](https://github.com/iOfficeAI/aionui-backend/commit/26918a04b265a73298e216bda504b79bd47c852a))
* **ci:** auto-update Cargo.lock in release-please PR ([a3d6147](https://github.com/iOfficeAI/aionui-backend/commit/a3d614713cf0999f2471472dcfa6a8af4f9c0b8f))
* **ci:** auto-update Cargo.lock in release-please PR ([91f4495](https://github.com/iOfficeAI/aionui-backend/commit/91f44956ed24c8cb370d4ea71d9f62cd29e09fe7))
* **ci:** resolve clippy warnings in aionui-api-types and aionui-realtime ([7b8c1c8](https://github.com/iOfficeAI/aionui-backend/commit/7b8c1c82976284b149195ae67707a1d62bf01f0f))
* **conversation:** kill agent process on conversation delete ([#267](https://github.com/iOfficeAI/aionui-backend/issues/267)) ([456ff32](https://github.com/iOfficeAI/aionui-backend/commit/456ff322845b96fd70583dcf1fc2fb12c2371030))
* **runtime:** include nvm node bins in startup path ([#261](https://github.com/iOfficeAI/aionui-backend/issues/261)) ([00c5762](https://github.com/iOfficeAI/aionui-backend/commit/00c57627592a567eb71fbc4edc564e2b579b86ee))


### Code Refactoring

* **acp:** replace first-message flag with PromptPipeline + hooks ([#262](https://github.com/iOfficeAI/aionui-backend/issues/262)) ([d1f3c95](https://github.com/iOfficeAI/aionui-backend/commit/d1f3c95eebea4053c45b56dcd973fe4e44f0fe6c))

## [0.1.1](https://github.com/iOfficeAI/aionui-backend/compare/v0.1.0...v0.1.1) (2026-05-13)


### Features

* **logging:** integrate aionrs independent file logging ([da16d97](https://github.com/iOfficeAI/aionui-backend/commit/da16d97975202808c2b24ea884dff6f43c2de4d3))
* **logging:** integrate aionrs independent file logging ([dc950c8](https://github.com/iOfficeAI/aionui-backend/commit/dc950c8781b3f5fdc4aaa435c9f69e27b079ccb2))


### Bug Fixes

* **office:** stabilize flaky port_timeout_on_no_listener test ([30df119](https://github.com/iOfficeAI/aionui-backend/commit/30df119eec0ae5b125b2613d4573b6432ed42094))
* revert console_layer to match main (remove .with_ansi(false)) ([e1dfe73](https://github.com/iOfficeAI/aionui-backend/commit/e1dfe73db029685bac99f2f293cfab586db1f0b1))
* **team:** remove 30s heartbeat polling from agent event loop ([752be98](https://github.com/iOfficeAI/aionui-backend/commit/752be981a487c1281fee48bf0b21d4d9c1574bbf))
* **team:** remove redundant 30s heartbeat polling from event loop ([88672eb](https://github.com/iOfficeAI/aionui-backend/commit/88672ebb59aa9eb25e3396ed312bd1d807df4e07))


### Code Refactoring

* **ai-agent,conversation:** move session ops, tighten visibility, fix idle scanner + backfill ACP metadata ([#254](https://github.com/iOfficeAI/aionui-backend/issues/254)) ([299c5d3](https://github.com/iOfficeAI/aionui-backend/commit/299c5d30e7674d91136139886c9b02a99b932515))


### Documentation

* **assistants:** add word-form-creator to preset-id-whitelist ([#252](https://github.com/iOfficeAI/aionui-backend/issues/252)) ([343b15b](https://github.com/iOfficeAI/aionui-backend/commit/343b15bc5ab362c566ae0d8e2ed61921d58b9497))
