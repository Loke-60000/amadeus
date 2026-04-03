WIP - Do not try to install if you do not know what you are doing.

For native Live2D builds, initialize the public Cubism Framework checkout with `git submodule update --init --recursive`.

Then, extract the proprietary Cubism Core from the [Live2D SDK download](https://www.live2d.com/download/cubism-sdk/download-native/) and set `AMADEUS_CUBISM_CORE_DIR` to the extracted `Core/` directory. Optionally place it at `Core/` in the project root (already git-ignored).

All sample adapter code is vendored into `src/core/native/cpp/`. The Framework is the only external build dependency beyond Core.