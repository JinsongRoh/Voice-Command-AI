# iOS localized permission texts

One folder per app language (`en`, `ko`, `ja`, `es`). Each `InfoPlist.strings`
translates the permission texts (`NS*UsageDescription`) of `../Info.plist`;
`en` repeats the English values. The languages match `CFBundleLocalizations`
in `../Info.plist` and the UI locales in `apps/ui/src/i18n`.

The generated Xcode project (`gen/apple`) is not in git. The Mac build script
copies these `.lproj` folders into `gen/apple/vca-mobile_iOS/` after
`tauri ios init`, where xcodegen picks them up as resources.

When you change a permission text in `../Info.plist`, change it here in all
four languages too. The format is `"KEY" = "value";`, UTF-8.
