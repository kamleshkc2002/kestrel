# Vendored AppImage tools

`scripts/build-appimage.sh` runs linuxdeploy with its GTK plugin. The plugin is
copied from this directory into the build's tools directory, so the AppImage
build fetches nothing but linuxdeploy itself.

- `linuxdeploy-plugin-gtk.sh` is an unchanged copy of the plugin the build runs.
  Upstream: <https://github.com/linuxdeploy/linuxdeploy-plugin-gtk>. License:
  MIT.
- `LICENSE.linuxdeploy-plugin-gtk` is the upstream license text, from the same
  repository.

## Updating

1. Copy the current `linuxdeploy-plugin-gtk.sh` from the upstream repository
   over the vendored file, without editing it.
2. Copy `LICENSE.txt` from the same revision over
   `LICENSE.linuxdeploy-plugin-gtk` if it changed.
3. Rerun the AppImage build (`bash scripts/build-appimage.sh 0.1.0`) and compare
   the bundled libraries with the previous build:

   ```bash
   find build/appimage/AppDir -name '*.so*' | sort
   ```

   A library that appears or disappears means the plugin now deploys something
   different; explain the difference or revert the update.
4. Start the artifact and confirm the window still opens.
