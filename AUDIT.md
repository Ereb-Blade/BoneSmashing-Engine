# Полный технический аудит Rust-движка

**Первичный аудит:** 2026-11-24

**Актуализировано:** 2026-11-27 по состоянию коммита `91a9bde`

## 1. Общая информация о проекте

Проект — однопроцессный 3D-движок на Rust:

- графический API: `wgpu 25`;
- окна и события: `winit 0.30`;
- UI редактора: `egui 0.32`;
- математика: `glam 0.34`;
- загрузка glTF/GLB: crate `gltf`;
- загрузка OBJ: crate `tobj`;
- изображения: crate `image`;
- сериализация сцен: `serde_json`;
- файловые диалоги: `rfd`;
- сборка: один исполняемый binary crate.

Проект уже значительно превратился в рабочее демонстрационное ядро. Реализованы сцена, редакторский UI, загрузка моделей, PBR, IBL, HDR, тени, скелетная анимация, стекло и локальные point/spot lights с редакторским gizmo.

Текущая общая оценка: **рабочая прототипная и демонстрационная база, но пока не production-ready для больших или сложных сцен**.

Ниже выводы сверены с текущим исходным кодом, включая локальное освещение, сценовую иерархию и viewport-иконки.

### Результаты проверки

- `cargo test`: пройдена, **144 теста**, 0 ошибок.
- Покрытие тестами включает scene hierarchy и parented lights, их сериализацию и загрузку тестовых сцен, viewport picking, one-step undo для перетаскивания света, animation sampling, импорт OBJ/glTF, mipmaps, IBL/environment, frustum culling, MSAA, shader validation и shadow queue.
- `cargo check --release`: проходит без предупреждений.
- `cargo clippy --all-targets --all-features -- -D warnings`: проходит.
- `cargo fmt --all -- --check` и `git diff --check`: проходят.
- Strict lint-проверка включена в GitHub Actions CI.
- Автоматические тесты проверяют поведение и компиляцию, но live-визуальная проверка значков в запущенном окне не проводилась; FPS и реальную GPU-производительность аудит не подтверждает.

## 2. Полная структура проекта

### Entry point и приложение

- `src/main.rs` — создание event loop и запуск `App`.
- `src/app.rs` — основной цикл приложения, управление рендером, вводом, импортом, анимацией и сценами.
- `src/core.rs` — встроенные идентификаторы мешей и материалов.
- `src/config.rs` — скорости камеры, near/far plane и depth format.
- `src/camera.rs` — камера первого лица и камера uniform.
- `src/input.rs` — клавиатура и накопленный mouse delta.
- `src/math.rs` — look-at, perspective, orthographic и normal matrix.

### Сцена

- `src/scene/world.rs` — объекты, model groups, иерархия и кэш матриц.
- `src/scene/entity.rs` — render object и связь с mesh/material/asset.
- `src/scene/transform.rs` — позиция, поворот и масштаб.
- `src/scene/io.rs` — JSON-файл сцены, валидация, миграции и загрузка.
- `src/scene/mod.rs` — публичный export типов сцены.

### Рендерер

- `src/renderer/state.rs` — центральный state рендерера и frame orchestration.
- `src/renderer/pipeline.rs` — WGSL-шейдеры и создание render pipelines.
- `src/renderer/mesh.rs` — CPU mesh format, встроенные меши и OBJ.
- `src/renderer/material.rs` — текстуры, mipmaps и параметры материалов.
- `src/renderer/gltf.rs` — glTF/GLB import и анимация.
- `src/renderer/environment.rs` — HDR environment, IBL maps и BRDF LUT.
- `src/renderer/debug_ui.rs` — весь редакторный UI, gizmo, history и диалоги.
- `src/renderer/mod.rs` — export модулей рендерера.

### Ресурсы и документация

- `assets/models/` содержит тестовые модели Fox, RiggedFigure, FlightHelmet и CarConcept.
- `assets/scenes/` содержит animation, local-light, showroom, benchmark и другие тестовые сцены.
- `assets/environments/dikhololo_night_2k.hdr` — ночное окружение CC0 для сцены локального освещения.
- `Dark grey.jpg` и `texture.png` — встроенные и тестовые текстуры.
- `README.md` — пользовательская документация; `AUDIT.md` — техническая оценка и ограничения.
- `Cargo.toml` / `Cargo.lock` — зависимости и lock-файл.

Директория `target/` содержит артефакты сборки и не является исходным кодом движка. Файлы `Dark grey.jpg` и `texture.png` находятся в корне проекта; небольшая организация репозитория стала бы чище, если разместить встроенные и тестовые текстуры внутри `assets/textures/`.

## 3. Текущая архитектура движка

Основная зависимость подсистем выглядит так:

```text
winit
  -> App
     -> World
     -> Camera / Input
     -> Renderer
        -> GPU resources
        -> Mesh / Material
        -> Environment
        -> RenderPipeline
     -> DebugUi
        -> egui
        -> UndoHistory
        -> диалоги и worker-потоки
```

Архитектура содержит следующие границы:

- `App` оркестрирует application loop и взаимодействие подсистем.
- `World` хранит scene objects и иерархию model groups.
- `Renderer` владеет GPU-ресурсами, batching и рендер-проходами.
- `DebugUi` управляет редактором и напрямую вызывает методы world и renderer.
- glTF/OBJ загружаются сначала в CPU-представление, затем загружаются в GPU.
- Файловые диалоги, импорт сцен и HDR использует фоновые потоки с `mpsc`.

Архитектурные сильные стороны:

- есть разделение сцены и рендерера;
- используются stable IDs для объектов и model groups;
- есть кэширование world matrices;
- scene I/O валидирует данные;
- есть undo/redo для ключевых изменений;
- фоновая загрузка не блокирует основной event loop;
- имя иерархии объектов отделено от mesh/material identity.

Архитектурные слабости:

- `App` хранит window, renderer, world, анимационные кеши и UI одновременно;
- `Renderer` является владельцем pipeline, resource state, frame state, shadow cache и environment cache;
- `DebugUi` имеет около 4.5 тыс. строк и сильно связан с World и Renderer;
- нет отдельного asset manager с lifetime и reference counting;
- нет ECS или устойчивого runtime API;
- нет render graph или явной модели frame dependencies;
- нет отдельной подсистемы physics.

## 4. Подробный разбор подсистем

### Оконная система

Используется современный `winit::ApplicationHandler`.

Поддержаны:

- создание окна;
- resize;
- redraw;
- close requested;
- focus lost;
- захват курсора;
- keyboard и mouse события.

При resize обновляется aspect камеры и внутренние render targets.

Создание окна обрабатывает ошибку через `Result`, пишет диагностику в stderr и завершает event loop без `.unwrap()`; startup panic path из первичного аудита устранён.

### Ввод

В [`src/input.rs`](src/input.rs) клавиатура представлена `HashSet<KeyCode>`, mouse delta накапливается как `(f64, f64)`.

Реализовано:

- pressed/released;
- накопленный mouse delta;
- cursor lock;
- очистка состояния при потере фокуса;
- Control/Shift/Escape/F1 shortcuts.

Не реализованы:

- action mapping;
- axis/input contexts;
- gamepads;
- rebinding;
- analog input abstraction;
- priority событий редактора и камеры.

### Камера

Камера хранит:

- eye position;
- yaw;
- pitch;
- up;
- aspect;

Матрица вида рассчитывается вручную через right-hand perspective matrix.

В контроллере camera forward вычисляется по yaw/pitch, pitch ограничен ±89°, движение относительно forward/right/up.

Это рабочая FPS-камера для демонстрации, но не готовая система для орбитальных, редакторских и split-screen камер.

### Сцена и scene graph

`World` хранит:

- `Vec<RenderObject>`;
- stable `ObjectId`;
- standalone objects;
- model groups;
- group parent links;
- local point/spot lights with optional model-group parents;
- glTF node metadata;
- animation playback;
- rotating object;
- local/world matrix caches;
- dirty sets.

World умеет:

- добавлять и удалять objects/groups;
- создавать и дублировать иерархию;
- reparent nodes;
- сохранять world transform при reparent;
- проверять cycle;
- отклонять shear-only reparent;
- сохранять мировую позу света при attach/detach;
- получать мировые position/direction источника через transform parent;
- удалять дочерние lights вместе с удаляемой model-group веткой;
- кэшировать world matrices;
- маркировать только затронутые descendants dirty.

Матрица объекта вычисляется как:

```text
model = parent_node_world или model_group_world * object_local
```

Это рабочая редакторская иерархия для раннего engine.

Потенциальная проблема масштабирования: всякий lookup world matrix всё ещё может быть связан с hierarchical traversal и внутренними HashMap/RefCell. Для десятков тысяч объектов это требует transform pass и dedicated hierarchy subsystem.

### Сериализация сцен

Формат:

- JSON;
- текущая версия формата 9;
- scene groups;
- до 16 локальных point/spot lights с optional `parent_group`;
- scene objects;
- camera;
- renderer settings;
- environment path;
- material overrides;
- animation settings.

Реализованы:

- max 8 MiB scene file;
- max 100 000 объектов;
- max 1000 model groups;
- max 256 assets;
- backward compatibility для старых форматов, включая сцены без `lights`;
- reject unknown fields;
- validation finite values;
- cycle detection;
- относительные asset paths внутри папки сцены;
- absolute paths за пределами папки проекта.

Сцена сохраняет camera, направленный и локальные источники света, exposure,
shadows, MSAA, render scale, HDR environment и animation playback state. Parent
света сериализуется индексом группы, а не runtime ID. Время анимации намеренно
не сохраняется.

Главный недостаток формата: он привязан к mesh/material numeric IDs и конкретным renderer settings. Более стабильный формат должен оперировать asset handles и named resources.

### Анимация

glTF анимация поддерживает:

- skeletons;
- joints;
- inverse bind matrices;
- Translation;
- Rotation;
- Scale;
- Step, Linear и CubicSpline;
- loop;
- speed;
- seek;
- animation clip selection.

Morph targets загружаются с POSITION/NORMAL/TANGENT displacements; веса применяются на CPU и загружаются в динамические vertex/tangent buffers. Поддержаны glTF weight animations, поиск и сброс в инспекторе; имена целей читаются из `mesh.extras.targetNames`, если они присутствуют.

Анимационные модели загружаются в `animation_models: HashMap<PathBuf, LoadedGltfModel>`. Сэмплинг выполняется только при изменении clip/time.

Поддерживаются базовые clip cross-fades и смешивание skeletal poses/morph weights. Не реализованы animation graph, state machine и layered blending.

### Редактор и UI

Debug UI включает:

- scene hierarchy;
- отдельную вкладку **Lights** с add/edit/delete и выбором родителя;
- кликабельные редакторские point/spot glyphs в viewport, hover-name и spot direction;
- object/group selection;
- material inspector;
- model inspector;
- transform controls;
- gizmo translation/scale/rotation;
- viewport picking;
- reparent;
- duplicate/delete/rename;
- animation controls;
- light controls;
- HDR environment;
- MSAA/render scale;
- undo/redo;
- scene open/save;
- profiler counters.

Light icons рисуются только egui overlay при открытом редакторе и не попадают
в игровой render path. Они не используют depth test, поэтому видны поверх
геометрии. Spot glyph показывает направление, но не масштаб cone/range.

Undo хранит до 32 snapshots. Для одной операции перетаскивания света создаётся
один undo step.

UI рабочий, но `debug_ui.rs` является очень крупным orchestration-модулем. Он зависит от world, renderer, scene I/O и egui одновременно.

## 5. Подробный аудит рендерера

### GPU backend

В [`Renderer::new`](src/renderer/state.rs):

- создаётся `wgpu::Instance`;
- создаётся surface;
- выбирается high-performance adapter;
- создаётся device и queue;
- выбирается sRGB surface format;
- создаётся depth, HDR и MSAA targets.

Выполняется базовая обработка lost/outdated surface.

Отдельно не реализованы:

- recovery после lost device;
- GPU health monitor;
- fallback backend UI;
- adaptive backend selection.

При поддержке необходимых adapter features доступны GPU timestamp queries для
кадра, shadow map и opaque pass; при отсутствии поддержки эти значения
показываются как unavailable.

### Render path от модели до экрана

1. glTF/OBJ загружается в CPU-модель.
2. Для glTF читаются nodes, skins, clips, images и materials.
3. Для OBJ читаются `.obj`/MTL/текстуры.
4. Меши загружаются в vertex/index/skin/tangent/UV буферы.
5. Изображения готовятся с mipmaps.
6. Материалы создают per-material bind groups.
7. `World` даёт world matrices объектов.
8. CPU формирует batches по `(MeshId, MaterialId)`.
9. Складываются instance matrices и skin matrices.
10. Выполняются shadow pass и main passes.
11. HDR результат tone-maps в surface.
12. egui рисуется поверх кадра.

### Buffers

Используются:

- camera uniform;
- light uniform;
- skin matrix storage;
- shadow matrix uniform;
- instance vertex buffer;
- per-material parameter uniform;
- vertex/index buffers;
- tangent/UV/skin buffers.

Instance и skin buffers grow до next power-of-two. Это простая и рабочая стратегия для прототипа, но не для системы streaming resources.

### Draw batching

Batches организованы как `HashMap<(MeshId, MaterialId), Vec<InstanceRaw>>`.

Плюсы:

- одинаковые mesh/material реальны батчатся;
- отдельные shadow batches;
- transparent objects имеют отдельную очередь;
- skinning передается через instance skin offset.

Риски:

- нет dedicated batch sorting structure;
- нет persistent GPU instance arena;
- нет indirect rendering;
- нет GPU culling;
- каждый transparent instance может стать отдельным draw batch;
- нет state sorting по depth/material/custom pass.

### Bind groups

Есть четыре основные группы:

1. shared camera/light/skin/shadow;
2. per-material textures/params;
3. environment IBL;
4. scene color для glass/postprocess.

Binding layout понятный и статический. Основная проблема — bind groups не инкапсулированы в resource descriptors и имеют много ручных повторов при resize/upload.

### Шейдеры

Основной shader находится в [`src/renderer/pipeline.rs`](src/renderer/pipeline.rs).

Реализованы:

- skybox;
- opaque PBR;
- transparent PBR;
- directional shadow;
- glass;
- postprocess.

PBR включает GGX, Schlick geometry, Fresnel, ambient IBL, direct light, normal map, occlusion, emissive и specular workflow.

### PBR и материалы

Поддержаны:

- base color;
- metallic/roughness;
- normal map;
- occlusion;
- emissive;
- transmission;
- volume thickness;
- IOR;
- clearcoat factors and textures;
- specular factor;
- specular color;
- specular-glossiness;
- unlit;
- alpha blend/mask;
- double-sided;
- до 4 UV sets;
- glTF texture transform.

Обнаружено ограничение: clearcoat уже импортируется и поддерживается в PBR shader, но sheen, iridescence, anisotropy и dispersion не реализованы. Такие расширения игнорируются с предупреждением; фактический материал может отличаться от оригинала.

### Textures и mipmaps

Текстуры поддерживают mipmaps для:

- base color;
- metallic/roughness;
- normal;
- occlusion;
- emissive;
- specular;
- specular color;
- clearcoat;
- clearcoat roughness;
- clearcoat normal;
- transmission;
- thickness.

GLTF-контент ограничен:

- 128 MiB raw image data;
- 128 MiB mipmap data;
- device max texture dimension;
- `MAX_IMPORTED_TEXTURE_DIMENSION = 4096`.

OBJ текстуры downscale до 4096 px и ограничены общими бюджетами 128 MiB для raw image data и 128 MiB для mipmaps. Пиковое потребление памяти на крупных наборах текстур отдельно не профилировалось.

### Форматы GPU-ресурсов и валидация данных

- Индексы в обоих импортерах хранятся как `Uint32`; массив индексов имеет тип `Vec<u32>` в [`src/renderer/mesh.rs`](src/renderer/mesh.rs), а glTF индексы конвертируются через `into_u32()` в [`src/renderer/gltf.rs`](src/renderer/gltf.rs). Поддержки `Uint16` для экономии индекса нет.
- OBJ проверяет u32 vertex count с `u32::try_from` и checked addition в [`src/renderer/mesh.rs`](src/renderer/mesh.rs). Также проверяются пустые triangles, incomplete normals/UV, out-of-range indices и non-finite vertex data.
- glTF проверяет количество и диапазоны индексов, mismatched vertex attributes и non-finite positions/normals/tangents/UV/colors/weights в [`src/renderer/gltf.rs`](src/renderer/gltf.rs).
- Нулевые glTF/OBJ меши отклоняются по empty position/index conditions.
- Матрицы glTF node и animation дополнительно проверяются на finite/invalid rotation/non-invertible skin node data.
- Большой формат индексов пока не приводит к явной переполненной нумерации, так как count/ranges проверены и индексный тип фиксирован, но без mesh splitting каждый primitive ограничен общим u32-диапазоном.
- В исходниках не найдены загрузчики KTX2, DDS или декодеры BCn/ASTC/ETC. Texture path раскодирует обычные изображения через crate `image`, конвертирует их в RGBA и загружает как `Rgba8UnormSrgb`, `Rgba8Unorm` или `Rgba16Float` environment.
- Сжатых текстур, которые могли бы существенно снизить VRAM, нет. Для большого 4K набора без KTX2/BCn бюджет и VRAM могут быть существенным ограничением даже при image/mipmap CPU-budget.

### HDR и tone mapping

Используется `Rgba16Float`.

Tone mapper — ACES approximation:

```text
HDR -> exposure -> ACES -> clamp -> surface
```

Gamma-aware mip generation реализована для base color/normal/data channels.

Отдельного централизованного color management/a color space policy пока нет.

### IBL и environment

Environment subsystem создаёт:

- equirectangular 2D environment;
- diffuse irradiance;
- prefiltered specular environment;
- BRDF LUT;
- studio fallback environment;
- loading fallback.

HDR/EXR loading выполняется в worker thread. Texture processing использует CPU importance sampling и diffuse convolution.

Загрузка environment корректно переключается только после готовности результата.

### Lighting

Реализовано:

- один key directional light;
- один fill directional light;
- ambient IBL;
- ambient top/bottom coefficients;
- до 16 локальных point/spot lights на сцену;
- дальностное затухание с мягким обрезанием по range;
- smooth cone для spotlights;
- локальная позиция/направление с преобразованием через parent group;
- сохранение и загрузка lights в формате сцены.

Локальные источники добавляются в shared light uniform и вычисляются
per-fragment в основном PBR path. Текущий shader перебирает все включённые
источники до фиксированного лимита; clustered/forward+ culling не реализованы.
Локальные lights не отбрасывают тени.

### Shadows

Используется одна directional shadow map 4096×4096.

Shadow pass:

- собирает casters независимо от camera frustum;
- исключает transmission glass;
- поддерживает cutout shadows;
- имеет отдельный shadow instance buffer;
- кэширует matrix/instances/batches;
- поддерживает выбираемое PCF число taps 2/4/8/16 (по умолчанию 8).

Directional shadow camera подстраивается под bounds сцены. Если валидных bounds нет, используется legacy fallback volume ±7.5. Остаются ограничения одной directional shadow map без cascades и без локальных теней.

### Frustum culling

Используется sphere-vs-frustum test по world center/radius.

Skinned meshes намеренно не culled.

Это рабочая начальная реализация. Не реализован точный AABB/frustum intersection и hierarchical culling.

### Occlusion culling

Не реализован.

### LOD

Не реализован.

### Instancing

Есть CPU instance batching по mesh/material, но не полноценный instancing as data-oriented system.

### Transparency

Transparent objects sorted back-to-front по camera-space depth bounds center.

Обычная alpha transparency рисуется отдельным pass после opaque.

Glass отделён от обычной alpha pass.

### Glass/refraction

Glass shader использует:

- IOR;
- Fresnel;
- transmission factor;
- screen-space background;
- volume attenuation;
- environment reflection;
- direct reflection;
- fixed refraction offset clamp.

Это screen-space approximation. Реальная трассировка лучей, вторая геометрия и multi-bounce refraction отсутствуют.

### Normal/tangent handling

Есть:

- tangent vertex buffer;
- UV frame;
- tangent handedness;
- normal map scale;
- back-face normal flip;
- CPU normal matrix;
- skin normal inverse-transform logic.

Это рабочая модель для типичных glTF моделей. Отрицательный scale и unusual skin transforms требуют дополнительной визуальной и математической проверки.

## 6. Найденные проблемы и риски

### P0 — потенциальные crash-пути

#### Window creation

Место: [`src/app.rs`](src/app.rs)
Статус: исправлено в этапе 1.

Ошибка создания окна теперь обрабатывается через `match`; текст ошибки выводится в stderr, а event loop завершается явно.

#### Surface capabilities

Место: [`src/renderer/state.rs`](src/renderer/state.rs)
Статус: исправлено в этапе 1.

Добавлены отдельные selectors для формата, alpha mode и present mode. Пустые capabilities считаются ошибкой и приводят к контролируемому отказу инициализации renderer.

#### Сворачивание окна и resize до нуля

Место: [`src/app.rs`](src/app.rs) и [`src/renderer/state.rs`](src/renderer/state.rs)

Статус: исправлено в этапе 1.


Для нулевого size введено явное состояние `surface_ready = false`. Resize(0, 0) не вызывает surface configure и render-target recreation; `Renderer::render` и redraw не acquire surface до восстановления положительного размера.

#### Необработанные uncaptured GPU validation errors

Место: создание device в [`src/renderer/state.rs`](src/renderer/state.rs)

Статус: частично исправлено в этапе 1.

Причина: большинство ошибок GPU передаются при создании device, но runtime validation errors могут происходить при texture/buffer/command creation или submission.

Последствия: ошибка может быть обработана только внутренним wgpu callback и может привести к процессному crash/panic или незаметному invalid frame.

Добавлен `device.on_uncaptured_error(...)` с выводом диагностики в stderr. Surface Lost, Outdated и Timeout обрабатываются reconfiguration. Error scopes остаются следующей задачей этапа инструментов.

### P1 — неполная реализация форматов

#### glTF extensions

Место: [`src/renderer/gltf.rs`](src/renderer/gltf.rs)
Статус: исправлено в совместимом режиме на этапе 1.

Clearcoat импортируется и поддерживается шейдером, включая factors/textures. Sheen, iridescence, anisotropy и dispersion остаются неподдержанными extensions.

Причина: file format allowlist шире actual rendering support.

Последствия: импортированный материал выглядит иначе или теряет extension effect.

Критичность: средняя.

Решение этапа 1: импорт не прерывается целиком; неподдержанное расширение игнорируется с диагностическим предупреждением, а базовый PBR-материал сохраняется. Полная реализация sheen/iridescence/anisotropy/dispersion остаётся отдельной задачей графического этапа.

### P1 — память текстур

#### OBJ texture budget

Место: [`src/renderer/mesh.rs`](src/renderer/mesh.rs)
Статус: исправлено в этапе 1.

Для OBJ добавлены общие лимиты 128 MiB на raw images и 128 MiB на mipmaps, помимо dimension 4096.

Причина: множество текстур суммарно могут потреблять много RAM/VRAM.

Последствия: allocation failure или OOM на больших OBJ сценах.

Критичность: средняя-высокая.

Остаточный риск: реальные пиковые RAM/VRAM на больших наборах текстур отдельно не измерялись.

### P1 — shadow scaling

Место: [`src/renderer/state.rs`](src/renderer/state.rs)

Directional shadow camera теперь автоматически подбирается по bounds сцены. Legacy fixed volume ±7.5 остаётся fallback, если нет валидных bounds.

Причина: fallback матрица не масштабируется по scene bounds; основной bounds-aware путь уже расширяет покрытие.

Последствия: fallback может обрезать удалённые объекты; основной путь всё ещё использует одну карту и не поддерживает cascades.

Критичность: средняя для open world.

Рекомендация: проверить качество bounds-based framing на разнообразных сценах и рассмотреть cascaded shadow maps.

### P1 — strict Clippy

Статус: исправлено в этапе 1.


Причина: style/API-shape debt.


### P1 — фоновые загрузки и race conditions

#### HDR environment

Место: [`src/renderer/state.rs`](src/renderer/state.rs) и [`src/renderer/debug_ui.rs`](src/renderer/debug_ui.rs)

Проблема: повторный запуск HDR blocked до завершения текущей загрузки через единственный `environment_load` receiver. В данном потоке кода старый результат не перезаписывает новый, потому что новый результат не может быть запущен до очистки receiver.

Причина: нет отмены или generation ID, но есть serialized single-loader policy.

Последствия:

- после закрытия окна/окончания сессии worker завершит загрузку и отправит сообщение в канал, который уже не читает; это не подтверждённая утечка, а стандартная disconnected send;
- при длительном HDR task пользователь не может отменить или заменить активную загрузку;
- если после закрытия сцены environment request остаётся pending, poll берет только предыдущее состояние конкретного loading slot.

Критичность: низкая-средняя.

Рекомендация: добавить generation ID/cancellation state и явную lifecycle-блокировку при close/shutdown. Для сложного UI заменить receiver на task handle или channel с request ID.

#### Scene/asset loading

Место: [`src/renderer/debug_ui.rs`](src/renderer/debug_ui.rs)

Проблема: UI временно отключает scene I/O и asset import, пока загрузка активна, поэтому параллельная загрузка нескольких сцен или моделей в основных ветках не создаётся пользователем.

Причина: single slot `scene_loading` / `asset_import`.

Последствия: close или смена состояния во время worker thread не приведёт к немедленной race по данным, но старые receivers могут быть отброшены при shutdown и нет явного join worker.

Критичность: низкая-средняя.

Рекомендация: сохранить явное состояние task/cancellation и дождаться worker или убрать его из lifecycle перед shutdown, если загрузка станет асинхроннее.

### P1 — выгрузка GPU-ресурсов

Место: [`src/renderer/state.rs`](src/renderer/state.rs)

Проблема: при discard/scene asset retain ненужные `Mesh`/`Material` удаляются из Vec и Rust-owned GPU resources могут быть освобождены при drop. Перекрывающийся bind group/textures lifecycle определяется вложенными владельцами этих типов.

Причина: нет централизованного reference-counted asset manager.

Последствия:

- обычное удаление/смена сцены выгружает только те assets, которые больше не перечислены в retained meshes/materials;
- сейчас нет подтверждённой утечки во внутренних HashMap/cache при этих операциях;
- при частом импорте и удалении без очистки scene retain/allocation policy есть возможность накопления GPU memory в динамических buffers и shadow cache;
- dynamic instance/skin buffers с next-power-of-two capacity не уменьшаются после удаления сцены.

Критичность: средняя.

Рекомендация: добавить asset reference counts/lifetime, explicit GPU resource accounting и shrink policy для dynamic buffers. Не считать это confirmed leak, пока не измерено VRAM across repeated import/delete cycles.

### P1 — память Undo и согласованность snapshots

Место: [`src/renderer/debug_ui.rs`](src/renderer/debug_ui.rs)

Проблема: хранится до 32 глубоких clones `World`, каждый snapshot содержит scene hierarchy, objects и caches.

Причина: World clone включает в себя CPU vectors/maps и RefCell caches. RenderSnapshot хранит только renderer settings и material params, а не GPU resource handles.

Последствия:

- память растёт линейно с размером World и зависит от глубины undo history;
- при повторных import/delete snapshots могут сохранять старые World copies и объекты, но GPU resources остаются renderer-owned и не клонируются;
- подтверждённой рассинхронизации с animation_models/GPU нет в типичных undo ветках, потому что world snapshots сохраняют stable IDs; удалённые asset resources не удаляются из renderer, а undo-восстановление использует те же IDs;
- при прямой замене сцены history очищается, а `animation_models` заменяется текущим map.

Критичность: средняя.

Рекомендация: измерять память и заменять deep snapshots на command-based history или structural scene diff для больших сцен.

### P2 — архитектура

#### Монолитный Renderer

Место: [`src/renderer/state.rs`](src/renderer/state.rs)

Проблема: один state хранит pipelines, resources, batches, cache, settings и async receiver.

Причина: ресурсы и frame orchestration добавлялись без выделения subsystems.

Последствия: сложнее тестировать и заменять части renderer.

Критичность: средняя.

Рекомендация: выделить resource manager, frame renderer и render graph позже.

#### Монолитный DebugUi

Место: [`src/renderer/debug_ui.rs`](src/renderer/debug_ui.rs)

Проблема: UI содержит scene editing, gizmo, undo, imports, dialogs и profiling.

Последствия: высокая связность и сложная расширяемость.

Критичность: средняя.

Рекомендация: выделить separate editor subsystem и UI panels.

#### Монолитный App

Место: [`src/app.rs`](src/app.rs)

Проблема: App хранит window, renderer, camera, world, UI, animation и asset caches.

Последствия: тяжело разделить runtime и editor.

Критичность: средняя.

Рекомендация: отделить `EngineRuntime`, `EditorState` и `AppLoop`.

### P2 — производительность

- все world objects проходят за frame preparation;
- batches пересобираются в каждом frame;
- transparent object сортируется как отдельный instance;
- skin matrices собираются в один CPU buffer;
- instance matrices переписываются при изменении state;
- нет persistent GPU instance buffers;
- нет GPU culling;
- нет LOD;
- нет occlusion culling;
- одна directional shadow map фиксированного разрешения 4096×4096, без cascades;
- environment preprocessing занимает CPU worker;
- GPU timestamps cover frame/shadow/opaque, но нет устойчивого benchmark baseline
  и профилей для каждой стадии.

## 7. Анализ производительности

### CPU узкие места

Наиболее вероятные:

- scene object traversal;
- world matrix cache propagation;
- batch map rebuilding;
- sorting transparent objects;
- animation sampling;
- egui prepare frame;
- stable object ID lookup;
- model hierarchy validation.

Для scenes >10k-100k объектов потребуется явный dirty transform pass и spatial acceleration.

### GPU узкие места

- draw call count;
- triangle count;
- MSAA;
- Rgba16Float frame copies;
- PCF с настраиваемым числом taps (default 8);
- glass extra passes/copies;
- skinning vertex shader;
- many material bind groups;
- full pass per transparent object;
- repeated instance buffer writes.

Нет данных для гарантированной оценки FPS.

### Инструментирование и неизмеренные параметры

В editor Performance panel доступны CPU UI/scene/acquire/submit timings,
GPU frame/shadow/opaque timings при поддержке adapter timestamps, draw calls,
triangles и scene counters. Это базовое live-инструментирование, не benchmark.

Не измерены воспроизводимые performance baselines, peak VRAM/RAM, стоимость
импорта и подготовки environment, а также масштабирование на фиксированных
наборах 10k/100k объектов. Без таких прогонов нельзя указывать конкретные FPS
или обещанные лимиты сцен.

## 8. Оценка готовности подсистем

| Подсистема | Оценка |
|---|---|
| Window/winit | Частично реализована |
| Input | Начальная реализация |
| Camera | Частично реализована |
| Scene World | Хорошо реализована |
| Scene hierarchy | Хорошо реализована |
| Scene serialization | Рабочая реализация |
| Undo/redo | Рабочая реализация |
| glTF/GLB import | Хорошо реализована |
| OBJ import | Частично реализована |
| Mesh upload | Рабочая реализация |
| Materials | Рабочая реализация |
| PBR | Частично реализована |
| IBL/HDR | Хорошо реализована |
| Direct lighting | Частично реализована |
| Локальные point/spot lights | Начальная реализация, до 16; без теней |
| Shadows | Частично реализована |
| Frustum culling | Частично реализована |
| Transparency | Частично реализована |
| Glass/transmission | Частично реализована |
| Skeletal animation | Хорошо реализована |
| Debug UI/editor | Частично реализована |
| GPU profiling | Базовая реализация; несколько pass timings при поддержке GPU |
| Physics/collision | Не реализована |
| ECS | Не реализована |
| Instancing system | Начальная реализация |
| LOD | Не реализована |
| Occlusion culling | Не реализована |
| GPU culling | Не реализована |
| Automated benchmarks | Не реализована |

## 9. Отсутствующие важные компоненты

1. Asset manager с stable handles.
2. ECS или formal runtime scene representation.
3. GPU resource manager.
4. Render graph.
5. LOD/mesh streaming.
6. Clustered/forward+ lighting для масштабирования за лимит 16 lights.
7. Тени от локальных lights; сейчас тени есть только у directional light.
8. Cascaded shadow maps.
9. GPU culling and indirect rendering.
10. Occlusion culling.
11. Animation state machine and layered blending; базовые cross-fades уже есть.
12. Physics/collision.
13. Central logger and diagnostic report.
14. Reproducible benchmark suite and performance baselines.
15. Asset hot reload.
16. Public engine API policy.

## 10. Архитектурные рекомендации

Не следует сейчас полностью переписывать:

- работающий World;
- работающий World matrix cache;
- работающий PBR shader;
- рабочий glTF importer;
- working scene format.

Следует разделить:

- `EngineRuntime`;
- `SceneRuntime`;
- `RenderSubsystem`;
- `AssetSubsystem`;
- `EditorSubsystem`;
- `GpuResourceManager`;
- `FrameProfiler`;
- `HistorySystem`.

Следующие изменения должны сохранять существующую функциональность поэтапно и проверяться тестами.

## 11. Дорожная карта разработки

### Этап 1 — Критические проблемы
Статус: выполнен.

- window creation обрабатывается через `Result`;
- surface format, alpha mode и present mode валидируются capability selectors;
- нулевой размер обрабатывается как suspended surface;
- surface Lost/Outdated/Timeout обрабатываются reconfiguration;
- uncaptured GPU errors выводятся в diagnostics callback;
- unsupported glTF extensions используют совместимый PBR fallback с предупреждением;
- OBJ import имеет общий 128 MiB raw image и 128 MiB mipmap budget;
- strict Clippy проходит;
- базовый CI добавлен для check/test/Clippy.

### Этап 2 — Архитектура

- разделить `App`;
- выделить `Renderer` subsystems;
- выделить `DebugUi` components;
- ввести resource descriptors;
- сократить high-arity constructors;
- ввести `FrameContext`;
- определить immutable scene/runtime boundaries.

### Этап 3 — Основные подсистемы

- AssetManager;
- handle-based resources;
- расширенная animation state machine/layered blending поверх имеющихся cross-fades;
- dedicated physics subsystem;
- formal input action system;
- scene validation improvements.

### Этап 4 — Графика

- cascade shadows;
- проверка bounds-based shadow framing и cascaded shadow maps;
- локальные shadows и масштабирование числа lights;
- improve glass/refraction;
- formal color management;
- advanced glTF material support;
- TAA только после baseline measurement;
- SSR только после workload evidence.

### Этап 5 — Производительность

- benchmark 10k/100k scenes;
- persistent instance buffers;
- dedicated batch structures;
- GPU frustum culling;
- indirect draw;
- spatial hierarchy;
- LOD;
- occlusion culling;
- benchmark-ами измерять GPU timestamps и результаты оптимизаций.

### Этап 6 — Инструменты разработчика

- расширение существующих CPU/GPU timings и scene counters;
- asset inspector;
- дополнительные draw call/triangle diagnostics;
- shader debug views;
- scene validation UI;
- crash/log diagnostics;
- screenshots and visual regression tests;
- benchmark CI.

## 12. Приоритетный список следующих задач

1. Создать воспроизводимый benchmark baseline для CPU/GPU и больших сцен.
2. Использовать текущие GPU timestamps для измерения отдельных render passes.
3. Проверить bounds-based directional shadow framing на сценах разных масштабов; затем оценить cascades.
4. Определить план локальных теней и clustered/forward+ lighting сверх лимита 16.
5. Разделить `Renderer::render` на named render passes.
6. Выделить resource/material descriptors и handle-based AssetManager.
7. Разделить `DebugUi` на editor components и панели.
8. Добавить persistent instance buffers и dedicated batching.
9. Реализовать GPU/occlusion/LOD culling только после измерения bottlenecks.
10. Расширить анимационную систему поверх существующих cross-fades.
11. После стабилизации runtime рассмотреть ECS.
12. Physics и collision реализовать в отдельной подсистеме.

## 13. Общая оценка текущего состояния

Проект уже содержит рабочие:

- window/render loop;
- PBR/IBL/HDR;
- shadows;
- MSAA;
- glTF/GLB/OBJ import;
- materials/textures/tangent frames;
- scene graph;
- skeletal animation;
- editor UI;
- point/spot lighting с scene hierarchy, parent transforms и clickable editor icons;
- undo/redo;
- scene JSON format;
- базовые CPU/GPU profiler timings и draw/scene counters;
- glass/transmission approximation.

Ключевые слабости:

- monolithic `App`, `Renderer` и `DebugUi`;
- одна directional shadow map без cascades и локальных теней;
- отсутствуют LOD, occlusion, GPU culling и indirect drawing;
- неполная support policy для glTF extensions;
- нет воспроизводимого benchmark/performance baseline;
- отсутствуют physics/ECS/asset manager.

**Итог:** фундамент движка рабочий и его можно развивать итеративно. Startup,
scene loading и текущие проверки проходят. Ближайшие инженерные задачи —
измеримый benchmark baseline, дальнейшее разделение renderer/editor modules и
планирование теней/масштабирования локального света; конкретные FPS без
воспроизводимых профилей утверждать нельзя.
