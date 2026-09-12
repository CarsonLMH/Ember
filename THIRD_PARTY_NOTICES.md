# Third-party notices

This document records the model, native-runtime, and image-fixture artefacts in
Ember that need explicit provenance or attribution. It is not an automatically
generated SBOM for every transitive Cargo and npm package; those exact versions
remain recorded in the repository's manifests and lockfiles.

The summaries below do not replace the linked licence texts.

## Bundled face models

### YuNet face detector

- Artifact: `src-tauri/models/face_detection_yunet_2023mar.onnx`
- SHA-256: `8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4`
- Copyright notice in the bundled licence: Copyright (c) 2020 Shiqi Yu
- Licence: MIT
- Source: [OpenCV Zoo, immutable model revision](https://github.com/opencv/opencv_zoo/blob/f12e12798e8314f7c074a6656816c048dcc95b7a/models/face_detection_yunet/face_detection_yunet_2023mar.onnx)
- Full text: [`src-tauri/models/LICENSE-yunet.txt`](src-tauri/models/LICENSE-yunet.txt)
- Licence-file SHA-256: `c83b8120c50ccbd4c4f96edf53141bdd566ebb8f8e9227e415326aa1b1aba958`

The local licence text is byte-for-byte identical to the [licence in the
pinned OpenCV Zoo directory](https://github.com/opencv/opencv_zoo/blob/f12e12798e8314f7c074a6656816c048dcc95b7a/models/face_detection_yunet/LICENSE).

### SFace face-recognition model

- Artifact: `src-tauri/models/face_recognition_sface_2021dec.onnx`
- SHA-256: `0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79`
- Upstream credit: SFace is contributed by Yaoyao Zhong; the directory README
  also credits Chengrui Wang for conversion work on its documented 2021sep
  export.
- Licence: Apache-2.0
- Source: [OpenCV Zoo, immutable model revision](https://github.com/opencv/opencv_zoo/blob/ba91a3b91d00d76e86540d4013f944bd6b514e39/models/face_recognition_sface/face_recognition_sface_2021dec.onnx)
- Full text: [`src-tauri/models/LICENSE-sface.txt`](src-tauri/models/LICENSE-sface.txt)
- Licence-file SHA-256: `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`

The local licence text is byte-for-byte identical to the [licence in the
pinned OpenCV Zoo directory](https://github.com/opencv/opencv_zoo/blob/ba91a3b91d00d76e86540d4013f944bd6b514e39/models/face_recognition_sface/LICENSE).
OpenCV Zoo applies Apache-2.0 to all files in that directory. The exact training
dataset and file-specific conversion history for the bundled 2021dec weights
are **UNVERIFIED** by the cited upstream material; no stronger claim is made.

See [`docs/MODELS.md`](docs/MODELS.md) for the complete provenance and
preprocessing contract.

## Statically linked inference runtime

### `ort` and `ort-sys` 2.0.0-rc.13

Ember uses the Rust `ort` and `ort-sys` crates at exactly `2.0.0-rc.13`.

- Licence: MIT OR Apache-2.0
- Source: [`pykeio/ort` tag `v2.0.0-rc.13`](https://github.com/pykeio/ort/tree/v2.0.0-rc.13)
- Full texts: [MIT](https://github.com/pykeio/ort/blob/v2.0.0-rc.13/LICENSE-MIT) and [Apache-2.0](https://github.com/pykeio/ort/blob/v2.0.0-rc.13/LICENSE-APACHE)

### Microsoft ONNX Runtime 1.28.0

The pinned `ort` release downloads or accepts a pre-supplied ONNX Runtime
library and Ember links it statically.

- Copyright: Microsoft Corporation and contributors
- Licence: MIT
- Source: [`microsoft/onnxruntime` tag `v1.28.0`](https://github.com/microsoft/onnxruntime/tree/v1.28.0)
- Full text: [ONNX Runtime 1.28.0 licence](https://github.com/microsoft/onnxruntime/blob/v1.28.0/LICENSE)
- Required upstream component notices: [ONNX Runtime 1.28.0 `ThirdPartyNotices.txt`](https://github.com/microsoft/onnxruntime/blob/v1.28.0/ThirdPartyNotices.txt)

**Release-packaging caveat:** this repository bundles the two model licence
files in the `.app`, but this documentation pass did not establish that a built
application bundle carries the full `ort`, ONNX Runtime, and ONNX Runtime
third-party notice texts. Before public distribution, inspect the final
artifact and include every notice required by the selected `ort`/ONNX Runtime
licences. Web links in this file are provenance records, not a substitute for
required offline licence material.

## Test image

### `src-tauri/tests/fixtures/astronaut.png`

- Subject: astronaut Eileen Collins
- Source: `skimage.data.astronaut` from scikit-image 0.20.0, originally from
  the NASA Great Images collection
- SHA-256: `88431cd9653ccd539741b555fb0a46b61558b301d4110412b5bc28b5e3ea6cb5`
- Rights statement: scikit-image records no known copyright restrictions and
  identifies the image as released into the public domain.
- Upstream documentation: [scikit-image 0.20 astronaut entry](https://scikit-image.org/docs/0.20.x/api/skimage.data.html#skimage.data.astronaut)
- Exact upstream file: [scikit-image tag `v0.20.0`](https://github.com/scikit-image/scikit-image/blob/v0.20.0/skimage/data/astronaut.png)

The repository copy's SHA-256 matches that tagged upstream file. It is used by
an ignored real-model integration test and is not a production photo fixture.

## Project-generated visual fixtures

These files are recorded for provenance; no third-party photographer or stock
library is claimed.

### `docs/assets/demo-coast.jpg`

- Generated specifically for Ember with OpenAI image generation on 2026-09-12
- Depicts fictional adults
- SHA-256: `a4edcc01fab0218007d59dad75f517cd330b7d20f3ded35948014ccff908d6fc`
- Separate third-party licence notice: none recorded; the maintainer includes
  it as a project fixture

### `docs/assets/ember-culling.jpg`

- Screenshot captured from the real Ember Tauri app by the repository's
  deterministic documentation harness
- Displays synthetic copies derived from `demo-coast.jpg`, so the pictured
  adults are fictional rather than maintainer photos
- SHA-256: `01caceb08b47077e407ad65e8a19504b1d5b561057b2207eecf2bfc56c828547`
- Separate third-party licence notice: none recorded; the maintainer includes
  it as a project screenshot

The local generation and privacy record is
[`docs/assets/README.md`](docs/assets/README.md). This notice does not make a
broader claim about copyrightability or grant rights beyond those the project
maintainer holds.
