# security-crypto pulls Tink, which references annotation-only classes.
-dontwarn com.google.errorprone.annotations.**
-dontwarn javax.annotation.**
-dontwarn com.google.crypto.tink.**
# zxing-android-embedded loads its camera/decoder classes reflectively.
-keep class com.google.zxing.** { *; }
-keep class com.journeyapps.barcodescanner.** { *; }
