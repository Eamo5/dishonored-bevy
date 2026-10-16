# bevy_ui_render 0.19.1, vendored

Bevy's UI renderer (MIT OR Apache-2.0, see LICENSE-MIT / LICENSE-APACHE), patched in through
`[patch.crates-io]` in the workspace's Cargo.toml. One change, marked "dishonored-bevy" in the
sources: for a camera compositing in sRGB (`CompositingSpace::Srgb`), every UI pipeline (nodes and
text, texture slices, box shadows, gradients, UI materials) is specialised with the `SRGB_OUTPUT`
shader def and puts its colour out gamma-encoded, as Bevy 0.19's sprite renderer already does.
The interface then blends in gamma space, as the original game's Scaleform interface does over
its finished frame. Cameras without the component render as upstream.
