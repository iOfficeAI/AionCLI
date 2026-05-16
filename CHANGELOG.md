# Changelog

## [0.1.4](https://github.com/iOfficeAI/aionui-backend/compare/v0.1.3...v0.1.4) (2026-05-16)


### Features

* **ai-agent:** log every CLI detection + add doctor subcommand ([#285](https://github.com/iOfficeAI/aionui-backend/issues/285)) ([5ef6d0a](https://github.com/iOfficeAI/aionui-backend/commit/5ef6d0a4d99345a502a9073dfdfa0d07cfa52a8c))
* **runtime:** full shell-style command in spawn logs ([#278](https://github.com/iOfficeAI/aionui-backend/issues/278)) ([dd51616](https://github.com/iOfficeAI/aionui-backend/commit/dd516165ae9e22fcb0573ae9d8d3aa094e54cff2))


### Bug Fixes

* **team:** model routing + schema unification + lazy warm mode persistence ([#286](https://github.com/iOfficeAI/aionui-backend/issues/286)) ([199a392](https://github.com/iOfficeAI/aionui-backend/commit/199a392caca600ef215bb2ae71bfd82bda7bb744))


### Performance Improvements

* **team:** lazy warm — only start agent processes on first message ([#282](https://github.com/iOfficeAI/aionui-backend/issues/282)) ([6281f31](https://github.com/iOfficeAI/aionui-backend/commit/6281f31ac6a2656c1af51891589770f4583e00c2))


### Code Refactoring

* **app:** extract CLI definitions to cli.rs ([#280](https://github.com/iOfficeAI/aionui-backend/issues/280)) ([5685d52](https://github.com/iOfficeAI/aionui-backend/commit/5685d5237b8f51c70e80895b1c654325c958196e))
* **app:** introduce commands/ module with layered bootstrap for subcommands ([#283](https://github.com/iOfficeAI/aionui-backend/issues/283)) ([1216597](https://github.com/iOfficeAI/aionui-backend/commit/12165971cfae61d85376c102ef9f9afc5a7c5bbf))
* **app:** replace argv sniffing with clap Subcommand for mcp-* helpers ([#277](https://github.com/iOfficeAI/aionui-backend/issues/277)) ([c3d137c](https://github.com/iOfficeAI/aionui-backend/commit/c3d137c9e5fdcb12e29d5ca7abd6a0585bbc6c8d))
* **app:** split monolithic lib.rs/main.rs into per-module files ([#284](https://github.com/iOfficeAI/aionui-backend/issues/284)) ([f3462cb](https://github.com/iOfficeAI/aionui-backend/commit/f3462cbb1d6d830a3a368a76b2d9ea6424f21b64))

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
