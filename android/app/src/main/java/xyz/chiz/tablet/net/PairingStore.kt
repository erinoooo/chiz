package xyz.chiz.tablet.net

import android.content.Context
import androidx.security.crypto.EncryptedFile
import androidx.security.crypto.MasterKey
import org.json.JSONArray
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.File

/** Paired-PC record (spec 11): encrypted with a Keystore AES-GCM key. */
data class PairedPc(
    val pcId: String,
    val name: String,
    val fingerprintHex: String,
    val secretB64: String,
    val hosts: List<String>,
)

/**
 * Encrypted pairing store. Keystore alias `chiz_pairing` (spec 11).
 * NOTE: EncryptedFile lives in androidx.security:security-crypto; add
 * `implementation("androidx.security:security-crypto:1.1.0-alpha06")` to
 * app/build.gradle.kts (left out of the default build to keep it lean until
 * the first device run wires it).
 */
class PairingStore(private val context: Context) {
    private val file = File(context.filesDir, "paired.json.enc")
    private val masterKey = MasterKey.Builder(context, "chiz_pairing")
        .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
        .build()

    private fun encryptedFile() = EncryptedFile.Builder(
        context, file, masterKey,
        EncryptedFile.FileEncryptionScheme.AES256_GCM_HKDF_4KB,
    ).build()

    fun load(): List<PairedPc> {
        if (!file.exists()) return emptyList()
        val bytes = ByteArrayOutputStream().also { out ->
            encryptedFile().openFileInput().use { it.copyTo(out) }
        }.toByteArray()
        val arr = JSONArray(String(bytes))
        return (0 until arr.length()).map { i ->
            val o = arr.getJSONObject(i)
            PairedPc(
                o.getString("pc_id"), o.getString("name"), o.getString("fingerprint"),
                o.getString("secret"),
                (0 until o.getJSONArray("hosts").length()).map { j -> o.getJSONArray("hosts").getString(j) },
            )
        }
    }

    fun save(pcs: List<PairedPc>) {
        val arr = JSONArray()
        for (p in pcs) {
            arr.put(JSONObject().apply {
                put("pc_id", p.pcId); put("name", p.name)
                put("fingerprint", p.fingerprintHex); put("secret", p.secretB64)
                put("hosts", JSONArray(p.hosts))
            })
        }
        encryptedFile().openFileOutput().use { it.write(arr.toString().toByteArray()) }
    }
}
