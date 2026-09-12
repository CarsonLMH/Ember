# Bundled face models

Ember bundles two ONNX models for its optional People index. Inference runs
inside the app on the Mac; the face pipeline does not send photos, crops, or
embeddings to a model service. The model files and their licence texts are
packaged as Tauri resources under `src-tauri/models/`.

This manifest was verified on 2026-09-12 against the files in this repository
and immutable upstream revisions.

## Artifact manifest

| Role | Bundled artifact | SHA-256 | Upstream source | Licence |
| --- | --- | --- | --- | --- |
| Face detection | [`face_detection_yunet_2023mar.onnx`](../src-tauri/models/face_detection_yunet_2023mar.onnx) | `8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4` | [OpenCV Zoo at `f12e127`](https://github.com/opencv/opencv_zoo/blob/f12e12798e8314f7c074a6656816c048dcc95b7a/models/face_detection_yunet/face_detection_yunet_2023mar.onnx) | MIT; [bundled text](../src-tauri/models/LICENSE-yunet.txt), [upstream text](https://github.com/opencv/opencv_zoo/blob/f12e12798e8314f7c074a6656816c048dcc95b7a/models/face_detection_yunet/LICENSE) |
| Face recognition embeddings | [`face_recognition_sface_2021dec.onnx`](../src-tauri/models/face_recognition_sface_2021dec.onnx) | `0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79` | [OpenCV Zoo at `ba91a3b`](https://github.com/opencv/opencv_zoo/blob/ba91a3b91d00d76e86540d4013f944bd6b514e39/models/face_recognition_sface/face_recognition_sface_2021dec.onnx) | Apache-2.0; [bundled text](../src-tauri/models/LICENSE-sface.txt), [upstream text](https://github.com/opencv/opencv_zoo/blob/ba91a3b91d00d76e86540d4013f944bd6b514e39/models/face_recognition_sface/LICENSE) |

The bundled licence files are byte-for-byte copies of the corresponding
OpenCV Zoo licence files at those revisions. The model hashes above also match
the objects served by OpenCV Zoo at those revisions.

### YuNet provenance

OpenCV Zoo describes `face_detection_yunet_2023mar.onnx` as a fixed-input-shape
YuNet detector and licenses the directory under MIT. Its README points to
Shiqi Yu's [`libfacedetection.train`](https://github.com/ShiqiYu/libfacedetection.train/blob/a61a428929148171b488f024b5d6774f93cdbc13/tasks/task1/onnx/yunet.onnx)
as model lineage and training documentation. That earlier `yunet.onnx` is not
byte-identical to the 2023mar OpenCV Zoo artifact, so it is a lineage source,
not the exact file Ember bundles. See the immutable [OpenCV Zoo YuNet
README](https://github.com/opencv/opencv_zoo/blob/f12e12798e8314f7c074a6656816c048dcc95b7a/models/face_detection_yunet/README.md)
for the upstream claims.

### SFace provenance and known gap

OpenCV Zoo credits SFace to Yaoyao Zhong and licenses the model directory under
Apache-2.0. The directory README identifies the upstream
[`zhongyy/SFace`](https://github.com/zhongyy/SFace) project and credits Chengrui
Wang for converting the documented 2021sep export. Ember bundles the later
`2021dec` file from that same OpenCV Zoo directory.

**UNVERIFIED:** the upstream material at the pinned revision does not establish
the exact training dataset or a file-specific conversion history for the
`2021dec` weights. The authoritative facts here are therefore limited to the
exact OpenCV Zoo artifact, its matching hash, the directory's Apache-2.0
licence, and the family-level credits in the [SFace
README](https://github.com/opencv/opencv_zoo/blob/ba91a3b91d00d76e86540d4013f944bd6b514e39/models/face_recognition_sface/README.md).
Do not describe these weights as independently provenance-audited or use their
output as proof of identity.

## Ember preprocessing contract

The embedding space is defined by the models **and** the preprocessing below.
Changing any of it is a model-generation change even when both ONNX files stay
the same.

- The detector receives the orientation-applied cached preview, never a new
  full-resolution decode in the culling path.
- The preview is uniformly scaled into a fixed 640×640 YuNet canvas, placed at
  the top left, with black padding on the right or bottom. Input is planar BGR
  `f32` in the 0–255 range.
- YuNet detections are decoded at strides 8, 16, and 32 and use greedy NMS at
  IoU 0.3. Rectangles and five landmarks are mapped back into preview space.
- Each kept face is aligned from its five landmarks to Ember's canonical
  112×112 template with a no-reflection similarity transform and bilinear
  sampling. Out-of-bounds samples are black. SFace input is again planar BGR
  `f32` in the 0–255 range.
- SFace produces a 128-value embedding, which Ember L2-normalizes before
  storage and cosine comparison.
- Both ONNX Runtime sessions are configured with one intra-op thread. Face
  indexing remains background work and is not part of the flip hot path.

The executable source of truth is
[`src-tauri/src/facedet.rs`](../src-tauri/src/facedet.rs) and
[`src-tauri/src/faces.rs`](../src-tauri/src/faces.rs).

## Compatibility identifiers

The current constants in `faces.rs` are:

- `PREP_VERSION = 1`. Bump it when letterboxing, landmark alignment, tensor
  layout, normalization, or another preprocessing change shifts embeddings.
- `MODEL_RELEASE = 1`. This is a monotonic compatibility rank, not Ember's app
  version. Bump it whenever either bundled model or the preprocessing changes.

At worker start, Ember computes the two actual model-file hashes once there is
face work to do. The DB generation identity is the pair of computed hashes plus
`PREP_VERSION`; `MODEL_RELEASE` decides which binary may own a shared database.
An older or equal-ranked binary with a different generation parks instead of
rewriting newer embeddings.

The compiled SHA constants are reference pins, not a hard startup integrity
gate: a mismatch currently logs a warning, while the computed hashes still
drive DB compatibility. A clean database can therefore register changed model
bytes at the current release rank. Release review must verify the bundled
hashes rather than assuming the app will reject a substitution.

## ONNX Runtime build and licence contract

Ember pins Rust crates `ort` and `ort-sys` to `2.0.0-rc.13`, with
`download-binaries`, `tls-native`, `ndarray`, and `std` enabled and default
features disabled. The [`ort` rc.13 release](https://github.com/pykeio/ort/releases/tag/v2.0.0-rc.13)
uses ONNX Runtime 1.28.

During a linking build, `ort-sys` first honors `ORT_LIB_PATH`. Without that
override, its build script selects a prebuilt distribution, downloads it when
it is not already in the platform cache, verifies the archive against the
SHA-256 recorded in its pinned [`dist.tsv`](https://github.com/pykeio/ort/blob/v2.0.0-rc.13/ort-sys/build/download/dist.tsv),
and links `onnxruntime` statically. A clean build therefore needs either
network access, the matching cache entry, or a compatible library supplied via
`ORT_LIB_PATH`.

The rc.13 distribution table contains an `aarch64-apple-darwin` package but no
`x86_64-apple-darwin` package. That is why the downloaded-prebuilt build is
Apple-Silicon-only. For Ember's current feature set, the default macOS archive
is the table's `coreml` row, pinned there as SHA-256
`6934874e2e953576d9c1db47ff1af39c62c4f4220dbe6f988e131f72879674c7`.
Ember does not enable or register the Core ML execution provider despite that
archive label; its sessions use the default CPU provider.

`ort` and `ort-sys` are dual-licensed MIT OR Apache-2.0. Microsoft ONNX Runtime
1.28 is MIT-licensed and carries its own third-party notices. Exact links and
the release-packaging caveat are recorded in
[`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md).

## Updating either model

1. Take the artifact and licence from an immutable authoritative source.
2. Recompute SHA-256 and update the filename, resource manifest, compiled hash,
   and this manifest together. Do not rely on a mutable `main` URL.
3. If preprocessing changes, update `PREP_VERSION`; for any model or
   preprocessing change, advance `MODEL_RELEASE` to a value greater than every
   released bundle.
4. Re-run the deterministic face tests, real-model fixture test, two-process
   ownership tests, privacy deletion tests, and the face-enabled performance
   gate before release.
5. Recheck upstream licence, provenance, and third-party notices instead of
   assuming a replacement file inherits this one's terms.
