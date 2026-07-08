# Memory: System-Class Refocus

Date: 2026-06-07

The active Helios direction is System-class KMDF + DeviceIoControl + Mesa Venus. The WDDM Display-Only Driver pivot is archived and should not drive new work unless the owner explicitly asks for a DOD/display experiment.

Primary reference: `docs/decisions/SYSTEM_CLASS_REFOCUS_2026_06_07.md`.

Active priorities:

- restore the old System-class driver VM/device setup;
- benchmark offscreen Venus rendering and fence/submit latency;
- improve async submit, interrupt/DPC fence completion, and blob mapping performance;
- treat windowed/DOD/scanout presentation as later integration work.

Keep `kmd/src/dxgk.rs`, the archived display docs under `docs/archive/` as historical reference material only.
