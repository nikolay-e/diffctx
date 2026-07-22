# dcbench instance index

Generated from `instances/`. Total: 372 instances, 15 repos.
All gold-annotated (263 LLM-reviewed 2026-07-22, 109 legacy imports).

| Repo | Instances | With nontrivial gold | string_indirection |
|---|---|---|---|
| gitpod | 37 | 6 | 0 |
| sentry | 37 | 5 | 0 |
| react-native | 35 | 7 | 0 |
| aspnetcore | 25 | 19 | 12 |
| gitlab-foss | 25 | 23 | 10 |
| home-assistant | 25 | 20 | 12 |
| polars | 25 | 17 | 7 |
| neovim | 24 | 14 | 5 |
| kubernetes | 23 | 20 | 8 |
| envoy | 22 | 19 | 17 |
| spark | 22 | 8 | 16 |
| plausible | 20 | 17 | 11 |
| firefox-ios | 18 | 12 | 9 |
| nextcloud | 17 | 6 | 7 |
| tokio | 17 | 8 | 0 |

## gitpod (37)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `0150cf8` | 15 | 7 | 0 |  | Introduce workspace classes |
| `02b4952` | 23 | 7 | 0 |  | Add full clone setting for prebuilds |
| `04f590d` | 33 | 12 | 1 |  | Organization onboarding welcome message |
| `17e83b9` | 63 | 6 | 0 |  | Remove IAM component |
| `1bc46bd` | 87 | 12 | 0 |  | Refactor preview env + image build (DevOps) |
| `1f06a53` | 90 | 0 | 0 |  | Update Gitpod client libraries (broad go.mod) |
| `21f546d` | 21 | 13 | 0 |  | JetBrains IDEs 2024.3 stable release |
| `26f7f5d` | 38 | 19 | 0 |  | Initializer info to insights API |
| `301f1b7` | 98 | 14 | 0 |  | Go 1.24.3 upgrade (wide shallow changes) |
| `329e565` | 31 | 4 | 2 |  | Switch registry-facade hostPort → nodePort (TF) |
| `3777268` | 65 | 12 | 2 |  | Rename registry-credential → refresh-credential |
| `41f47c8` | 42 | 7 | 0 |  | Add Azure DevOps integration |
| `478a75e` | 2397 | 1 | 0 |  | Switch license to AGPL (ultimate stress test) |
| `4a7a7ab` | 95 | 5 | 4 |  | Remove registry-facade (binary .gz file) |
| `52848de` | 40 | 23 | 0 |  | Org-wide maintenance mode |
| `55b486e` | 27 | 18 | 0 |  | Introduce max_parallel_running_workspaces |
| `6b2187a` | 39 | 13 | 0 |  | Activity-based prebuilds |
| `6d93dd8` | 214 | 9 | 0 |  | Rename ws-sync → ws-daemon (file renames) |
| `7094f19` | 177 | 15 | 3 |  | Add collaborator role (SpiceDB → UI full stack) |
| `7172d82` | 141 | 12 | 0 |  | Fold ws-manager-node into ws-daemon |
| `756c5b0` | 78 | 4 | 0 |  | Replace Status by JetBrains Launcher |
| `79b75ab` | 40 | 15 | 0 |  | Add phone verification (full-stack) |
| `7f43d48` | 48 | 6 | 2 |  | Introduce multi-org behind feature flag |
| `82d786e` | 147 | 11 | 0 |  | Remove ws-scheduler (15+ dirs) |
| `836c620` | 68 | 8 | 0 |  | Migrate observability mixins (Jsonnet) |
| `8f643e7` | 147 | 8 | 0 |  | Add Java+Kotlin to public API (4 languages) |
| `99cc66b` | 39 | 7 | 0 |  | Re-create workspace pods on rejection |
| `a0e4fa6` | 179 | 3 | 0 |  | Remove replicated (IaC mega-removal) |
| `a303660` | 29 | 9 | 0 |  | Add insights page |
| `ad4b7a8` | 33 | 15 | 0 |  | Introduce org-level GITPOD_IMAGE_AUTH |
| `cdc11be` | 62 | 17 | 0 |  | GCP Terraform install (deep module hierarchy) |
| `d54bd04` | 261 | 18 | 0 |  | Enterprise onboarding settings (cross-stack) |
| `da4cafd` | 57 | 15 | 0 |  | Gitpod OIDC Identity Provider |
| `dd50c2a` | 19 | 17 | 0 |  | Cleanup UpdateOrganizationSettings API (5 langs) |
| `e9aae6e` | 27 | 15 | 0 |  | Auto-login dockerd with GITPOD_IMAGE_AUTH |
| `ec6b911` | 38 | 8 | 0 |  | Simplify image-builder-mk3 init containers |
| `f580e6b` | 85 | 8 | 0 |  | Hook scrubber as logrus formatter (broad) |

## sentry (37)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `0e503e1` | 120 | 5 | 0 |  | Split monolithic model files into modules |
| `1314277` | 286 | 4 | 0 |  | Change testing to in-memory router (286 test files) |
| `1853fe2` | 90 | 6 | 0 |  | Remove Biome, restore ESLint/Prettier |
| `2a0cf6e` | 47 | 10 | 0 |  | gRPC-Web service for SCM integrations (Protobuf!) |
| `2f0a302` | 29 | 12 | 0 |  | Add dotagents skills (17 renames, new dir hierarchy) |
| `363e7dc` | 19 | 13 | 0 |  | OAuth 2.0 Device Authorization (RFC 8628) |
| `3da7380` | 309 | 8 | 0 |  | Remove visual snapshot testing infrastructure |
| `42384da` | 288 | 2 | 0 |  | Rename CSS breakpoints to t-shirt sizing (cross-stack) |
| `45fc05c` | 245 | 5 | 0 |  | Replace Prettier with Biome |
| `4b97dee` | 56 | 8 | 0 |  | Enable Knip dead-code detection in CI |
| `4e71bc1` | 45 | 3 | 0 |  | Checkout restructure (31 renames) |
| `5447b01` | 12 | 6 | 0 |  | OAuth UI (balanced 6 Python + 6 TS/React) |
| `6082b7f` | 94 | 11 | 0 |  | Split trace node class into modules (5K+ lines moved) |
| `680d622` | 19 | 12 | 0 |  | Sandbox infrastructure for AI agents |
| `6d21808` | 17 | 6 | 0 |  | Remove legacy devservices setup (ClickHouse XML) |
| `726337e` | 74 | 7 | 0 |  | Rename performance_issues → detectors (51 renames) |
| `737d9c3` | 11 | 10 | 0 |  | Search bar boolean tags (5 languages incl. PEG.js) |
| `757cf20` | 13 | 11 | 0 |  | Rename otlp/ → eap/ (6 renames) |
| `848efcc` | 54 | 7 | 0 |  | Migrate Jest tests to SWC-safe mocking |
| `9458371` | 21 | 7 | 1 |  | Bump Node.js to v22.16.0 |
| `a4565db` | 8 | 6 | 0 |  | New splash loader (5 non-TS types) |
| `a8d6180` | 112 | 5 | 0 |  | Squash Django migrations (12K+ insertions) |
| `b3e9f66` | 72 | 4 | 1 |  | Replace internal devtoolbar with @sentry/toolbar |
| `d265446` | 680 | 5 | 0 |  | Migrate black/isort/flake8 → ruff (stress test) |
| `d4e4b74` | 19 | 15 | 0 |  | OAuth Device Flow (Django + HTML templates + TS) |
| `d68ed8b` | 138 | 5 | 0 |  | Streamline Storybook stories (large frontend restructure) |
| `dc4fb35` | 35 | 12 | 0 |  | Remove Overwatch subsystem (3,416 lines deleted) |
| `dc56fdd` | 13 | 10 | 0 |  | Uptime test endpoint (full-stack feature) |
| `e1c55b8` | 83 | 4 | 0 |  | Switch Jest → Vitest with SWC |
| `eaa7bd3` | 203 | 5 | 3 |  | Fix grouping hints for client-set in_app values |
| `f352de0` | 108 | 2 | 0 |  | Remove moment.js dependency |
| `f4fa54e` | 127 | 4 | 3 |  | Exception Groups (PEP 654) for grouping |
| `f6263b2` | 105 | 0 | 0 |  | Run Prettier on YAML/JSON/Markdown |
| `f7aa1be` | 28 | 7 | 0 |  | Migrate Yarn v1 → pnpm |
| `f8386e3` | 74 | 4 | 1 |  | Replace RunSQL → SafeRunSQL across 74 migrations |
| `fd69ceb` | 21 | 10 | 0 |  | New form system (new dir tree, 15 new files) |
| `fea5b1b` | 21 | 11 | 0 |  | First phase of uv package manager rollout |

## react-native (35)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `03b8b09` | 43 | 14 | 0 |  | Rename jsinspector-modern → fuseboxEnabled |
| `07abfce` | 59 | 1 | 0 |  | Remove template from react-native package |
| `15909fa` | 175 | 4 | 2 |  | Rename react-native-gradle-plugin → gradle-plugin |
| `1cb0a33` | 59 | 13 | 0 |  | Add react-native-test-library package |
| `2932c0f` | 40 | 14 | 1 |  | Implement baseline alignment on new arch |
| `2ce3686` | 40 | 8 | 0 |  | Fix RNTester iOS unit and integration tests |
| `332355e` | 48 | 12 | 0 |  | Implement UI consistency mechanism for JS thread |
| `37375d8` | 38 | 12 | 0 |  | Bump Folly to 2024.10.14.00 |
| `3aeba22` | 35 | 10 | 1 |  | Integrate IntersectionObserver in Event Loop |
| `42d6745` | 142 | 10 | 0 |  | Move .m → .mm for ObjC/C++ header compatibility |
| `4dea635` | 62 | 6 | 2 |  | Fork hermes/inspector as inspector-modern (Graphviz, PDF) |
| `59101d6` | 59 | 8 | 0 |  | Remove OSSLibraryExample |
| `610b14e` | 80 | 8 | 0 |  | Move min iOS version to 13.4 (Bazel+Xcode+CocoaPods) |
| `62e1110` | 138 | 12 | 0 |  | Bridgeless → Runtime rename (138 files) |
| `64c4e38` | 37 | 11 | 2 |  | Implement Long Tasks API for PerformanceObserver |
| `66df63d` | 26 | 26 | 0 |  | Generalize feature flags script |
| `6ba8b65` | 53 | 14 | 0 |  | Remove legacy ReactNativeConfig abstraction |
| `8ccaf00` | 42 | 10 | 0 |  | IntersectionObserver behind feature flag |
| `9526406` | 30 | 10 | 0 |  | Fix crash with nested FlatLists |
| `95ed8a6` | 47 | 8 | 0 |  | Merge all core codegen into FBReactNativeSpec |
| `972c2c8` | 23 | 5 | 0 |  | Bump Kotlin 1.9.x → 2.0.x |
| `98a38cc` | 31 | 8 | 0 |  | Convert text span package Java → Kotlin |
| `aefefdb` | 51 | 6 | 0 |  | Bump Folly to 2023.08.07.00 |
| `b365e26` | 32 | 9 | 0 |  | iOS blur filter via SwiftUI (new dirs) |
| `b7191cd` | 53 | 6 | 1 |  | Move TurboModule to internal.turbomodule namespace |
| `cf914e4` | 177 | 11 | 0 |  | Autolinking with support for linking projects (RNGP) |
| `cf926a1` | 35 | 11 | 1 |  | Decouple commit from mount on Android |
| `d16f0f2` | 26 | 14 | 0 |  | Decouple event logger from PerformanceEntryReporter |
| `d2c48f3` | 30 | 5 | 0 |  | Remove experimental_ prefix from mixBlendMode |
| `db80d78` | 31 | 6 | 0 |  | Merge .so libraries into libreactnative.so |
| `e7ce4ff` | 24 | 15 | 0 |  | Move NetworkReporter (renames) |
| `ee9ef3b` | 29 | 10 | 0 |  | Add Performance Issues to Perf Monitor |
| `f0f71ea` | 137 | 16 | 0 |  | Move packages/helloworld → private/helloworld |
| `f140c49` | 14 | 9 | 0 |  | Use Hermes V1 as default engine |
| `fcd6303` | 13 | 13 | 0 |  | Dismiss button ripples through all platforms |

## aspnetcore (25)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `1ce2228` | 129 | 9 | 0 | y | Move unified validation APIs to separate package — 100+ file package extraction |
| `223fe49` | 19 | 7 | 0 |  | Obsolete RazorRuntimeCompilation — pure C# attribute sweep |
| `2a0f0e0` | 10 | 7 | 1 |  | Rename EnvironmentBoundary to EnvironmentView — rename of #19's API |
| `32ee668` | 17 | 9 | 2 | y | Get rid of package baseline — MSBuild plumbing + docs + code |
| `33557cf` | 86 | 10 | 3 | y | Add Components.Testing E2E test infrastructure — code + MSBuild + solution files |
| `43b81a9` | 25 | 8 | 0 | y | Generalize Image component; add Video and FileDownload components |
| `4a634b9` | 17 | 10 | 2 |  | Add async form validation in Blazor — impl+2 PublicAPI baselines |
| `52ea667` | ? | 11 | 1 | y |  |
| `5b2e38c` | 50 | 9 | 2 | y | Add Blazor WebAssembly Service Defaults template — max extension diversity |
| `65468ce` | 27 | 10 | 3 |  | TempData support for Blazor — cross-project feature with csproj edits |
| `86ea7a5` | 16 | 11 | 3 | y | .NET side of Blazor SSR client-side validation (companion to #3) |
| `8854c58` | 23 | 9 | 1 | y | Align Blazor gateway and templates with Aspire — compact-but-diverse |
| `8883b98` | 15 | 9 | 1 |  | Add IPersistentComponentStateSerializer\<T> extensibility interface |
| `926090b` | 21 | 7 | 3 |  | Mark WebHostBuilder class as obsolete — cross-project cs-only sweep |
| `a751caa` | ? | 9 | 4 | y |  |
| `ae22b8b` | 16 | 10 | 1 |  | Remove long-obsolete MVC APIs — API removal + baseline churn |
| `b40cc0b` | 17 | 10 | 3 |  | Refactor HtmlRenderer layering — architectural refactor with baseline |
| `b8fd63a` | 161 | 4 | 0 |  | Mark API from 8 as shipped — pure PublicAPI baseline churn stress |
| `c0c2230` | 16 | 10 | 5 |  | SupplyParameterFromSession support for Blazor — src+razor+PublicAPI.Unshipped+tests |
| `d57ef96` | 14 | 10 | 0 |  | Rename ComponentPlatform to RendererInfo — API rename across projects |
| `e4d20da` | 28 | 9 | 2 | y | API-review rename: pause/resume methods + PersistentState attribute across cs/ts/js |
| `f5c1f08` | 9 | 6 | 2 |  | Add built-in EnvironmentBoundary component — compact feature |
| `f7bd408` | 20 | 8 | 3 |  | Remove obsolete APIs from Components — 5 PublicAPI baselines touched |
| `f905beb` | 5 | 8 | 4 | y | Rename LinkPreload to ResourcePreloader — compact rename with baseline |
| `fc8deca` | 779 | 6 | 0 | y | Rename Microsoft.AspNetCore.Testing to InternalTesting — repo-wide mega-rename stress |

## gitlab-foss (25)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `05b2de6` | 7 | 9 | 4 | y | Group-cluster deployments UI — compact, 4 ext types |
| `073c8b2` | 11 | 10 | 4 | y | GraphQL notes-in-discussions — graphql+models+lib |
| `07559fd` | 12 | 9 | 3 | y | Extract MR widget into separate endpoint — compact, 5 ext types |
| `0f59d73` | 5 | 7 | 2 | y | Rename epic state column to state_id — migration+schema+spec |
| `0f6c42c` | 20 | 9 | 2 |  | Multiple issue boards EE-to-CE move — controllers+models+services+routes |
| `387a4f4` | ? | 9 | 4 | y |  |
| `4aa76dd` | 63 | 9 | 2 | y | Remove dead MySQL code — cross-cutting removal over models/lib/config/db |
| `4b9b2a4` | 25 | 9 | 2 |  | GraphQL emoji add/remove/toggle mutations — graphql+models+lib |
| `525edec` | 5 | 5 | 0 |  | cluster_id FK on deployments — migration+model+spec coupling |
| `5e8f16b` | 7 | 7 | 1 | y | Cycle analytics Haml templates migrated to Vue |
| `7350eb1` | 14 | 10 | 1 |  | Wiki title search — model+lib+Haml views co-change |
| `80b2c3c` | 275 | 9 | 1 |  | Mirror-sync mega-commit — 275 files, 11 ext types, full stack incl. migrations |
| `83a8b77` | 21 | 12 | 2 |  | Namespace/ProjectStatistics GraphQL types |
| `85609c1` | 37 | 11 | 1 | y | CI variables of type file — migration+schema+models+controllers+views+JS |
| `911701a` | 9 | 7 | 1 |  | Extract discussion notes into new Vue component |
| `a00a23c` | 21 | 10 | 3 |  | GraphQL mutations for managing Notes |
| `a5aa40c` | 29 | 10 | 1 |  | Add Job specific variables — full-stack: migration+schema+model+service+Haml+Vue |
| `d745ff0` | 13 | 6 | 1 |  | Add username to deploy tokens — migration+schema+model+service+Haml views |
| `d8bb8d4` | 13 | 9 | 1 |  | Repository tree fetched via GraphQL frontend |
| `df3d936` | 9 | 8 | 3 | y | Last-commit data via GraphQL — schema-to-frontend ripple, 6 ext types |
| `e46d4bf` | 14 | 9 | 2 |  | Extract Git::{Base,Tag,Branch}HooksService — service extraction |
| `e7ee84a` | 24 | 11 | 2 | y | CI DAG support — migration+schema+models+services+lib+specs |
| `f5cde3a` | 8 | 6 | 1 |  | Rename tags to topics — 6 ext types across views/styles/model |
| `f93f8f5` | 102 | 6 | 0 |  | frozen_string_literal sweep in lib/gitlab — 100+ file stress |
| `fff7754` | 9 | 7 | 1 |  | Rename Repository table to PoolRepository — migration+schema+model rename |

## home-assistant (25)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `212cecd` | 1 | 2 | 1 |  | Improve typing for Mikrotik — 1-file typing change inside 1 of ~2000 identical integrations |
| `21e51b8` | 31 | 8 | 2 | y | New integration added whole: components/google_health/ + tests + requirements_all.txt + .coveragerc/ |
| `2384efa` | 8 | 6 | 2 | y | Add LLM integration — tiny new component wrapping existing helpers/llm.py core module; component vs  |
| `2707686` | 4 | 4 | 0 | y | Rename Modbus integration to "Manual Modbus" — manifest/strings/generated-config rename churn |
| `2ed386f` | 69 | 5 | 1 |  | Migrate to async_get_current_platform everywhere — core helper API migration touching ~65 integratio |
| `35397b8` | 2 | 4 | 2 |  | Deprecate device_tracker battery_level property — component base-class change all tracker integratio |
| `3b25b4c` | 6 | 6 | 0 | y | New helpers/service.py function to get single loaded config entry — core helper addition + first con |
| `3ba52a4` | 2 | 5 | 3 |  | Compact gem: Rain Bird options update listener — correct context is rainbird module tree + config_en |
| `6d55c07` | 19 | 6 | 2 |  | Mechanical multi-integration sweep (A-P): serial port listing via USB helpers across ~15 unrelated i |
| `8f465cf` | 8 | 9 | 1 | y | Remove deprecated Snapcast group entities and custom services — services.yaml + snapshots + strings  |
| `9dd1a59` | 3 | 4 | 1 | y | Rename "advanced" step to "additional" in Anthropic — config-flow step id: code identifier ↔ strings |
| `b7b147c` | 1 | 3 | 2 |  | Single-file dnsip config flow tweak using core helper add_suggested_values_to_schema — 1-file change |
| `b7f1171` | 5 | 6 | 1 |  | Rename Overseerr integration to Seerr — user-facing domain rebranding across manifest + strings + ge |
| `bc6060f` | 6 | 7 | 1 | y | Remove deprecated VoIP call_in_progress binary sensor — entity removal touching integration + string |
| `c50d064` | 34 | 8 | 4 | y | Mark integrations single_config_entry in manifest [a-i] — manifest.json-only sweep across ~30 integr |
| `c829331` | 63 | 2 | 0 |  | Use builtin TimeoutError [a-d] — wide shallow mechanical sweep across 60+ alphabetical integrations |
| `cb021f0` | 8 | 9 | 1 |  | Core extension point: integrations contribute serial port scanning helpers — usb component + helper  |
| `cb35849` | 4 | 8 | 4 | y | SMLIGHT reconfiguration flow — config-flow code + strings.json + snapshot coupling in one integratio |
| `d1275c1` | 3 | 5 | 2 | y | Deprecate CONCENTRATION_* constants in homeassistant/const.py — core constants used by hundreds of i |
| `d325f67` | 11 | 6 | 1 |  | Deprecate TargetSelectorData in favor of TargetSelection — helpers/target.py API change rippling int |
| `d40eeee` | 1 | 3 | 2 | y | Remove deprecated ConfigSource from homeassistant/core.py — core interface change; context should be |
| `e056c7d` | 5 | 7 | 2 |  | Deprecate openSenseMap air quality entity — deprecation repair-issue pattern: code + strings.json +  |
| `ee2fb6e` | 15 | 8 | 1 | y | Add config flow to SMTP integration — legacy-YAML-to-config-flow conversion, strings.json + manifest |
| `fd21674` | 20 | 8 | 0 |  | Add MELCloud Home integration — new domain adjacent to existing melcloud; name-collision stress for  |
| `ff3a801` | 655 | 2 | 0 |  | Add empty line after module docstring [a-d] — 655-file zero-semantic formatting sweep; selector shou |

## polars (25)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `1322669` | 4 | 6 | 2 |  | Rebalance deep merge_sorted chains — optimizer fix verified by py tests |
| `3b6875a` | 8 | 7 | 1 | y | Add `null_on_oob` in {Expr/Series}.gather — compact FFI gem, rs+py+pyi |
| `3d5a70b` | 123 | 7 | 0 |  | Move async executor to new polars-async crate — crate split stress |
| `447664f` | 20 | 12 | 1 |  | Add Rust backend for Expr.has_nulls — moves py-side impl into crates |
| `4695423` | 36 | 13 | 1 |  | Add pl.Expr.(min\|max)_by — dual-method FFI feature commit |
| `52eea50` | 17 | 10 | 1 | y | Implement dt.days_in_month — temporal namespace across boundary |
| `566d388` | 13 | 9 | 2 | y | Implement mean in `arr` namespace — polars-ops kernel + py test/doc |
| `6f59975` | 6 | 7 | 1 | y | Add len method to arr — minimal full-chain FFI commit |
| `79e4108` | 3 | 4 | 1 |  | Rolling aggregations with window_size=0 — polars-compute fix, py-side test |
| `855b3fc` | 20 | 8 | 0 |  | Binary serialization of LazyFrame/DataFrame/Expr as default — serde across FFI |
| `8fc67c7` | 110 | 9 | 0 |  | Move Buffer/SharedStorage to polars-buffer crate — crate split stress |
| `9472f7d` | 19 | 11 | 1 |  | Add `Expr.is_sorted` — new expression across Rust/Python boundary |
| `a298fc9` | 29 | 10 | 1 | y | Add `list` expression — Rust core + PyO3 + python API + stubs + docs in one commit |
| `a314cb6` | 86 | 9 | 0 |  | Scheduled removal of deprecated functionality — mass py+rs co-change |
| `a7d6cb7` | 36 | 10 | 1 |  | Add Expr.is_empty — expression added in polars-plan, exposed via py-polars |
| `aa77238` | 17 | 10 | 1 | y | Add Expr.cat.to and Expr.cat.physical — full FFI chain rs→pyi |
| `b35bc7b` | 63 | 6 | 0 |  | Move rolling to polars-compute — kernel relocation + workspace coupling |
| `b36e3f4` | 69 | 9 | 1 |  | Move (almost) all join code polars-core → polars-ops |
| `c70ec74` | 23 | 11 | 1 | y | Add `truncate` expression for numerics — rs core + py wrapper + docs |
| `c7896b9` | 101 | 2 | 0 |  | Rename POOL to RAYON — 101-file cross-crate identifier rename |
| `d54df64` | 3 | 4 | 1 |  | skip_batches bool-negation fix — Rust-only change, tests live on Python side |
| `d8b617e` | 171 | 9 | 0 |  | Split `py-polars` crate → polars-python extraction, 88 crates/ + 81 py-polars/ files |
| `da19eb8` | 4 | 6 | 2 |  | Expose fixed-size rolling window exprs in Python visitor — rs visitor + py cuda test |
| `f33ad61` | 46 | 11 | 0 |  | Implement {Expr,Series}.rolling_rank() — FFI feature + Cargo.toml/lock coupling |
| `fcbf169` | 4 | 6 | 2 |  | panic→error sorting object dtype — polars-core fix, py-side tests |

## neovim (24)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `01ea42c` | 6 | 7 | 1 | y | refactor(vim.secure): move to lua/secure.c — Lua-C interop relocation |
| `19b25f3` | 7 | 9 | 2 |  | feat(api): deprecate nvim_buf_add_highlight() — C + Lua + 4 doc files |
| `2234b84` | 7 | 8 | 1 |  | docs(generators): bake into cmake — build system + generator scripts |
| `2a7d0ed` | 136 | 4 | 1 |  | refactor: iwyu — repo-wide include restructuring (stress) |
| `2f85bbe` | 11 | 9 | 0 | y | feat!: rewrite TOhtml in lua — vimscript-to-Lua migration, compact-diverse |
| `34d808b` | 14 | 11 | 3 |  | feat(api): combined highlights in nvim_eval_statusline() — C↔Lua boundary + docs |
| `523e679` | 3 | 7 | 4 | y | refactor(matchparen): rewrite matchparen plugin in Lua |
| `5c92b40` | ? | 8 | 2 |  |  |
| `65b40e6` | ? | 5 | 0 |  |  |
| `6bf2a6f` | ? | 5 | 0 |  |  |
| `7028922` | ? | 4 | 2 |  |  |
| `71ac4db` | ? | 5 | 1 |  |  |
| `737f58e` | ? | 5 | 0 |  |  |
| `98f8224` | ? | 6 | 1 | y |  |
| `ae82636` | ? | 3 | 2 |  |  |
| `b280d57` | ? | 5 | 1 | y |  |
| `c822a26` | ? | 5 | 1 |  |  |
| `ce718e3` | ? | 6 | 0 |  |  |
| `d0af4cd` | ? | 5 | 1 |  |  |
| `de5cf09` | ? | 7 | 0 |  |  |
| `e80d191` | ? | 5 | 0 |  |  |
| `ead5683` | ? | 6 | 0 |  |  |
| `fcd1d97` | ? | 6 | 0 |  |  |
| `fd51fb3` | ? | 7 | 0 |  |  |

## kubernetes (23)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `078f462` | 106 | 9 | 7 | y | Workload API + Pod WorkloadReference generated files: 102/106 generated (openapi json/yaml/pb, clien |
| `09a1abd` | 54 | 5 | 3 |  | New CBOR library vendored: go.mod/go.sum churn across staging modules + vendor/ payload alongside th |
| `1acfb8e` | 11 | 7 | 1 | y | Hand-written half of DRA DeviceTaintRule v1 API: types, registry strategy, validation, apiserver wir |
| `1d4f88b` | 187 | 3 | 1 | y | vendor: bump runc to v1.2.1 — 183/187 vendor tree churn, essentially zero hand-written signal |
| `29601b8` | 44 | 8 | 1 | y | ResourcePoolStatusRequest API types + generated code in one commit: ~12 hand-written types/validatio |
| `29fad38` | 61 | 5 | 1 | y | Move endpointslice reconciler from pkg/controller to staging endpointslice repo: 34 pkg + 25 staging |
| `3902f56` | 45 | 5 | 3 | y | KEP-5491 list-attribute fields "make update" output: 45/45 files match generated/openapi/testdata pa |
| `45dfb46` | 6 | 8 | 2 |  | TokenRequestServiceAccountUIDValidation feature gate + UID validation logic: pkg/features + pkg/regi |
| `4bdaf6c` | 11 | 6 | 2 |  | Auto-generated files from ./hack/update-codegen.sh only — small all-generated commit, minimal viable |
| `4e592f6` | 136 | 7 | 1 | y | DRA API version rename s/v1beta2/v1/: mass rename with generated ripple, 121/136 generated/openapi/t |
| `5505c01` | 35 | 6 | 1 |  | Promote MutatingAdmissionPolicy to v1: new types.go plus 33/35 generated (deepcopy, conversions, cli |
| `566dc7f` | 26 | 10 | 2 | y | DRA device taints graduate to beta: gate flips + controller + registry + admission + staging + integ |
| `81c0b9c` | 3 | 3 | 0 |  | DRAListTypeAttributes alpha feature gate: minimal 3-file gate addition (features go + versioned_kube |
| `8ad3397` | 15 | 8 | 2 |  | Graduate SELinuxMount to GA: 7 hand-written gate/kubelet changes + 8 generated (openapi json, proto) |
| `99dbd85` | ? | 5 | 5 |  |  |
| `b7c4f21` | ? | 8 | 4 |  |  |
| `be5d632` | ? | 5 | 5 |  |  |
| `d43dc1a` | ? | 5 | 5 |  |  |
| `db9fcfe` | ? | 5 | 0 |  |  |
| `eaee6b6` | ? | 7 | 1 |  |  |
| `ec84379` | ? | 2 | 0 |  |  |
| `ee8c265` | ? | 5 | 1 |  |  |
| `f8e8e55` | ? | 5 | 1 |  |  |

## envoy (22)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `152db6c` | 55 | 7 | 0 | y | Extension restructure: migrate all istio extensions to contrib (api protos moved with code+docs) |
| `27916fa` | ? | 5 | 0 |  |  |
| `2a5ad79` | 12 | 6 | 1 | y | Runtime guard removal no_extension_lookup_by_name: proto + registry code + 9 test files |
| `2ca496c` | 34 | 5 | 1 | y | Bazel dependency rename to bzlmod names — 30 BUILD files across source/test/bazel/contrib |
| `33a4dee` | 14 | 6 | 1 | y | Runtime guard removal use_eds_cache_for_ads + legacy code paths (source+test+docs+changelog) |
| `37b63bd` | 8 | 5 | 1 | y | Bazel refactor of cargo workspace for builtin Rust extensions (Starlark+Cargo coupling) |
| `394e3c7` | 20 | 8 | 3 | y | New http filter_chain filter: 4 api proto files + source + test + docs + changelog + tools |
| `506746a` | 28 | 10 | 5 | y | New file server filter: api + 13 source + 7 test + docs + bazel |
| `57e0f60` | ? | 13 | 2 |  |  |
| `68d760a` | ? | 10 | 3 |  |  |
| `7155714` | ? | 5 | 2 | y |  |
| `7bc6bb9` | ? | 11 | 5 | y |  |
| `82cd127` | 48 | 7 | 2 |  | API-change ripple: remove StateType param through 45 call sites in source/contrib/mobile |
| `a2fe7fb` | 29 | 12 | 5 | y | New dynamic_modules tracing extension (C++/C/Rust + api + docs + changelog) |
| `a3c41b0` | 14 | 9 | 3 | y | MCP JSON REST bridge filter config: compact api+source+test+changelog commit |
| `a3ea604` | 23 | 10 | 4 | y | New sse_to_metadata HTTP filter for stream parsing (full PROTO→CODE→DOCS chain) |
| `aa4fe5e` | ? | 10 | 5 | y |  |
| `b0b550f` | 73 | 9 | 3 |  | Context refactor moving secret/ssl-context managers to server context — 48 test + 16 source + contri |
| `b3b748d` | 40 | 10 | 3 | y | New dynamic_modules formatter extension — polyglot C++/C ABI/Rust SDK + proto + docs |
| `b4d354e` | ? | 9 | 3 | y |  |
| `c97cd05` | 4 | 4 | 0 | y | Minimal runtime-flag deprecation report_load_with_rq_issued (2 source + test + changelog) |
| `f724ec1` | 6 | 8 | 2 | y | ext_authz shadow mode: tiny commit spanning api proto + source + test + changelog |

## spark (22)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `00f1057` | 12 | 8 | 0 | y | Add `quote` builtin function touching Scala, Java, Python, SQL golden files, docs |
| `0666f5c` | 10 | 9 | 2 | y | Add bit/octet_length APIs to Scala, Python and R simultaneously |
| `128fb13` | 11 | 8 | 0 | y | Add connect-client-jdbc module: pom.xml + code + CI coupling |
| `1e7e47d` | 12 | 8 | 0 | y | Add `array_prepend` function: Scala + Python + SQL tests + docs |
| `2113f10` | 13 | 8 | 0 | y | Introduce TVF `collations()`, remove SHOW COLLATIONS (grammar + code + docs) |
| `25a14c3` | 20 | 9 | 2 | y | Add date time functions to Scala, Python and Connect (part 1) with golden files |
| `29ef893` | 217 | 10 | 0 | y | Move AnalysisException to sql/api: 13 scala files + 203 golden .out files (noise stress) |
| `434aa30` | 29 | 10 | 0 | y | Move ArtifactManager from Spark Connect into SparkSession (sql/core) |
| `4d13c22` | 4484 | 8 | 0 | y | Move connect server and common to builtin module (extreme-stress mega-restructure) |
| `6537153` | 106 | 10 | 0 | y | Split common-utils Java code into new module (build + 100-file restructure) |
| `747846b` | 19 | 10 | 0 | y | Add `map_sort` function across SQL/Scala + PySpark + R + Connect (cross-language parity) |
| `89041a4` | 16 | 10 | 0 | y | Add from_xml/schema_of_xml to SQL, PySpark and Spark Connect + docs |
| `9126356` | 13 | 10 | 0 | y | Remove the Types Framework feature flag (flag removal ripples through SQL) |
| `97241eb` | 14 | 8 | 0 | y | Move pyspark.ml.remote to pyspark.ml.connect (pure Python package rename) |
| `a3930d3` | ? | 8 | 1 |  |  |
| `c0a1ea2` | ? | 3 | 0 |  |  |
| `c46d4ca` | ? | 5 | 2 |  |  |
| `c57556c` | ? | 0 | 0 |  |  |
| `cb59383` | ? | 9 | 2 | y |  |
| `d082ad0` | ? | 5 | 1 | y |  |
| `d429653` | ? | 5 | 1 |  |  |
| `f84cca2` | ? | 9 | 3 |  |  |

## plausible (20)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `0e84b52` | ? | 12 | 3 |  |  |
| `176234f` | ? | 11 | 1 |  |  |
| `359438a` | ? | 10 | 3 | y |  |
| `39656f7` | ? | 11 | 1 | y |  |
| `5358197` | ? | 12 | 1 |  |  |
| `592d2b2` | ? | 13 | 1 | y |  |
| `7eb7d9a` | ? | 10 | 1 | y |  |
| `810e48d` | ? | 6 | 1 |  |  |
| `8a0ec56` | ? | 10 | 0 | y |  |
| `8e546b8` | ? | 13 | 1 |  |  |
| `9de1532` | ? | 10 | 1 | y |  |
| `a07aaa6` | ? | 14 | 1 | y |  |
| `a9dc4ea` | ? | 12 | 0 | y |  |
| `bd40e49` | ? | 14 | 1 | y |  |
| `cc06a35` | 20 | 8 | 2 |  | Remove virtual rollups for persisted consolidated sites — refactor with module deletion ripple |
| `d298071` | 16 | 10 | 2 | y | Funnel exploration logic + UI prototype — Elixir query modules + HEEx + React/JS in one commit |
| `db9bcf3` | 11 | 8 | 1 | y | Recognize new sources and AI Assistants channel — includes Ecto migration + code in one commit |
| `ea3d23d` | 6 | 7 | 1 |  | Strict order funnels — Ecto schema field coupled with ClickHouse funnel query logic |
| `fd72fb8` | 4 | 5 | 1 |  | Conversion rate calculation in exploration funnel API — compact ex+js cross-boundary |
| `fdb561b` | 24 | 8 | 0 |  | Move invitations logic under Teams context — context-module move with alias ripple across 16 lib fil |

## firefox-ios (18)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `131c075` | 2 | 2 | 0 | y | Add "App Icon" string to InfoPlist.strings — minimal localization-only change |
| `1f932af` | 67 | 8 | 0 | y | Quick Answers feature project rename — mass file moves + pbxproj + test plan + CI yaml |
| `2439e77` | 14 | 7 | 3 | y | Rename onboarding "modern" identifiers to "base" (classes + file renames) — identifier/file rename c |
| `2a44c75` | 26 | 6 | 0 |  | Rename SearchEngines to SearchEnginesManager — symbol rename fan-out across call sites |
| `2c25273` | 82 | 6 | 1 |  | Redux rename of `activeScreens` state terminology — large cross-cutting Redux refactor with project  |
| `2cb089a` | 13 | 6 | 2 |  | Fix 1Password/Adjust/SDWebImage linking via Client-Bridging-Header — Swift↔ObjC bridging header + Sw |
| `3b445e8` | 6 | 9 | 3 | y | Translations: add bridge between webviews — new bridge component wired into project |
| `3de9b6c` | 11 | 8 | 3 |  | Move URLSessionProtocol into BrowserKit package — app→package source migration with project file |
| `4d69766` | 6 | 8 | 2 |  | Redux action migration part 1: add ModernAction to Redux package — package API + consumer coupling,  |
| `752ea8e` | 12 | 8 | 0 | y | Add 1Password/LastPass support with telemetry (focus-ios) — compact but maximally diverse extension  |
| `87dcc16` | 25 | 9 | 4 | y | Remove toolbar position feature flag, migrate addressBarMenu — flag removal ripple with nimbus yaml |
| `8c36775` | ? | 9 | 1 | y |  |
| `ac04ad6` | 12 | 5 | 0 | y | Replace/rename Onboarding checkmark assets — asset catalogs + binary resources + Swift references |
| `b54d1c5` | 10 | 6 | 2 |  | Remove Adjust SPM package — dependency removal coupling Package.resolved + pbxproj + call sites |
| `d7c8fda` | 15 | 7 | 2 |  | New TestKit SwiftPM library, move XCTestExtension into it — package birth + source relocation |
| `e3b1c7c` | 32 | 3 | 0 |  | Localization string import for v149 — pure .strings ripple linked via project file |
| `e942689` | 123 | 7 | 1 |  | Move Shared framework to BrowserKit part 1 — stress module extraction into package, Xcode project +  |
| `f8d47d5` | 50 | 6 | 1 | y | Disable ObjC inference, add `@objc` to all runtime-exposed selectors — cross-cutting Swift/ObjC inte |

## nextcloud (17)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `2bb8c72` | 15 | 9 | 1 | y | files_sharing trusted-server-shares toggle — PHP controller/capability + Vue UI coupling |
| `336c6d2` | 11 | 8 | 0 |  | new OCP email-address validator — lib/public + lib/private impl + tests + autoload |
| `4bbcbc5` | 13 | 10 | 0 |  | ISharedStorage made public API — OCP interface + apps/dav, files_sharing, files_versions implementat |
| `5b4adf6` | 42 | 9 | 1 |  | Move OC_Defaults to OCP\Defaults — lib/private→lib/public move rippling into apps + templates |
| `754422a` | 54 | 10 | 0 | y | theming app migrated to TypeScript + Vue 3 — full-stack stress (PHP backend + frontend rewrite) |
| `78fd649` | 16 | 10 | 0 |  | remove long-deprecated methods from OCP — lib/public + lib/private impls + tests + autoload |
| `79184f3` | 15 | 10 | 0 | y | settings setup checks migrated to Vue — compact-but-diverse (6 extensions) |
| `805fe3e` | 17 | 12 | 0 | y | files: filename-sanitization UI after WCF — full-stack PHP+Vue+TS+appinfo XML |
| `81752fc` | 117 | 3 | 0 |  | `Override` attribute added to all OCP classes — API-wide mechanical stress across lib/public |
| `9680004` | 31 | 8 | 1 | y | remove deprecated IServerContainer uses in lib/private — DI-graph ripple across 17 private classes + |
| `b035766` | 19 | 12 | 0 |  | preview migration + DB layout simplification — core/Migrations + lib/private/Preview + autoload nois |
| `b0df06d` | 36 | 8 | 0 | y | remove deprecated jQuery/jQuery UI — mixed frontend deletion (js+scss+assets+php) |
| `d5c23db` | 31 | 6 | 0 |  | Move CappedMemoryCache to OCP — private→public move + apps/user_ldap, files_external, files_sharing  |
| `d717dd9` | 24 | 8 | 0 |  | OCP Consumable vs Implementable API attributes — 21 lib/public files + autoload regeneration |
| `e7859f0` | 5 | 5 | 2 | y | DB migration for email setting — core/Migrations + provisioning_api controller + User + autoload, co |
| `f033ef7` | 12 | 10 | 2 |  | migrate all OCP\Template uses to ITemplateManager — OCP change rippling into 2 apps + core + base.ph |
| `f94fb33` | 5 | 6 | 3 |  | Move IToken and IProvider::getToken to OCP — compact private→public interface promotion |

## tokio (17)

| SHA | Files | Gold | NT | SI | Description |
|---|---|---|---|---|---|
| `0284d1b` | 4 | 5 | 1 |  | macros: make `select!` budget-aware — declarative macro + coop module coupling 2 hops away |
| `048049f` | 3 | 5 | 3 |  | rt: move `task::Id` into its own file — compact move, context lives behind `pub use` re-exports |
| `1204da7` | 10 | 10 | 2 |  | rt: split `runtime::context` into multiple files — module split, thread-local plumbing |
| `159a3b2` | 33 | 7 | 0 |  | rt(unstable): remove alt multi-threaded runtime — large deletion with scheduler dispatch fallout |
| `17cc283` | 4 | 5 | 1 |  | macros: accept path as crate rename — tokio-macros change + tokio test call sites + tests-build |
| `218f262` | 24 | 7 | 0 |  | rt: move I/O driver into `runtime` module — io → runtime relocation, many import-path hops |
| `3b5a15d` | 14 | 7 | 0 |  | fs: use Cargo feature for io-uring support instead of cfg — feature-flag restructure, Cargo.toml + c |
| `4b96af6` | 4 | 7 | 3 |  | macros: add "local" runtime flavor — tokio-macros entry + tokio/src/task/local.rs coupling |
| `5b4cbbc` | 11 | 5 | 0 |  | tokio: raise MSRV to 1.71 — many-crate shallow bump, toml + CI + code |
| `9730317` | 12 | 7 | 1 |  | time: move DelayQueue to tokio-util — cross-crate move tokio → tokio-util, Cargo.toml + code couplin |
| `a7bb054` | 4 | 4 | 0 |  | tokio: update stream, util, test to 2021 edition — 3 crates in one commit, shallow toml touches |
| `a8b6353` | 12 | 6 | 0 |  | rt: move Inject to `runtime::scheduler` — internal module move with re-export chain updates |
| `af6c87a` | 12 | 6 | 0 |  | chore: upgrade remaining 2018 edition crates to 2021 — 7 workspace crates + CI, wide shallow change |
| `d1f1499` | 24 | 7 | 0 |  | tokio: use cargo feature for taskdump instead of cfg — cfg→feature migration across runtime internal |
| `ea30a5e` | 3 | 3 | 0 |  | time: rename `cached_when` to `registered_when` — compact rename inside timer wheel internals |
| `ebeb78e` | 16 | 11 | 1 |  | rt: split internal `runtime::Handle` concerns — struct split threaded through schedulers |
| `ee1f0c4` | 10 | 8 | 1 |  | util: remove tokio-stream dependency from tokio-util — inter-crate dependency decoupling across code |
