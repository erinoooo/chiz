package xyz.chiz.tablet.net

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioManager
import android.media.SoundPool
import android.os.Build
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import java.io.File
import java.security.MessageDigest
import java.util.Locale
import java.util.UUID

/**
 * Audio-first feedback (spec 7). Owned by the service, never per-utterance.
 *
 * - 8 earcons as 16-bit 44.1 kHz mono WAVs in res/raw, peak -6 dBFS, <=200 ms,
 *   preloaded into one SoundPool (max 4 streams), USAGE_GAME /
 *   CONTENT_TYPE_SONIFICATION (follows the media volume; never the
 *   accessibility stream).
 * - One TextToSpeech, warmed with a silent utterance, QUEUE_FLUSH, user
 *   speech rate, utterance volume = feedback volume.
 * - Speech cache: on `profile`, synthesize each speak text (+ toggle
 *   on/off phrases) via synthesizeToFile, keyed by hash of
 *   text/engine/locale/rate; play from SoundPool; LRU 20 MB; live-speech
 *   fallback until cached.
 * - No haptics anywhere (spec rule 2).
 */
class AudioEngine(private val context: Context) {
    // res/raw ids (WAVs shipped by the app bundle).
    private val earconNames = listOf("fire", "hold_on", "hold_off", "on", "off", "blocked", "connect", "disconnect")
    private var soundPool: SoundPool? = null
    private val earconIds = mutableMapOf<String, Int>()
    private val cachedIds = mutableMapOf<String, Int>()
    private var tts: TextToSpeech? = null
    private val cacheDir: File = File(context.cacheDir, "speech").apply { mkdirs() }

    var feedbackMode: String = "both" // both | earcons | speech | off
    var speechRate: Float = 2.0f
    var volume: Float = 0.7f
    var ttsReady = false
        private set

    fun start(onTtsReady: () -> Unit) {
        val attrs = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_GAME)
            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
            .build()
        soundPool = SoundPool.Builder().setMaxStreams(4).setAudioAttributes(attrs).build()
        for (name in earconNames) {
            val id = context.resources.getIdentifier(name, "raw", context.packageName)
            if (id != 0) earconIds[name] = soundPool!!.load(context, id, 1)
        }
        tts = TextToSpeech(context, { status ->
            if (status == TextToSpeech.SUCCESS) {
                ttsReady = true
                tts!!.setSpeechRate(speechRate)
                // Cache completions land in the pool; live speech otherwise.
                tts!!.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
                    override fun onStart(id: String?) {}
                    override fun onError(id: String?) {}
                    override fun onDone(id: String?) {
                        if (id?.startsWith("cache:") == true) {
                            val f = File(cacheDir, id.removePrefix("cache:") + ".wav")
                            if (f.exists()) {
                                val sid = soundPool?.load(f.absolutePath, 1) ?: 0
                                if (sid != 0) synchronized(cachedIds) { cachedIds[f.nameWithoutExtension] = sid }
                            }
                        }
                    }
                    @Deprecated("deprecated")
                    override fun onError(id: String?, e: Int) {}
                })
                // Warm-up silent utterance (spec 7).
                tts!!.speak("", TextToSpeech.QUEUE_FLUSH, null, "warmup")
                onTtsReady()
            }
        })
    }

    fun setRate(rate: Float) {
        speechRate = rate.coerceIn(0.5f, 4.0f)
        tts?.setSpeechRate(speechRate)
    }

    fun earcon(name: String) {
        if (feedbackMode == "speech" || feedbackMode == "off") return
        val id = earconIds[name] ?: return
        soundPool?.play(id, volume, volume, 1, 0, 1.0f)
    }

    fun speak(text: String) {
        if (feedbackMode == "earcons" || feedbackMode == "off") return
        if (playCached(text)) return
        liveSpeak(text)
    }

    private fun liveSpeak(text: String) {
        val params = android.os.Bundle().apply {
            putFloat(TextToSpeech.Engine.KEY_PARAM_VOLUME, volume)
        }
        tts?.speak(text, TextToSpeech.QUEUE_FLUSH, params, UUID.randomUUID().toString())
    }

    /** Pre-synthesize profile speech on `profile` arrival (spec 7 cache). */
    fun precache(texts: List<String>) {
        val t = tts ?: return
        if (!ttsReady) return
        // Prime the pool with files already synthesized earlier.
        for (text in texts) {
            val f = cacheFile(text)
            if (f.exists() && !cachedIds.containsKey(cacheKey(text))) {
                val sid = soundPool?.load(f.absolutePath, 1) ?: 0
                if (sid != 0) cachedIds[cacheKey(text)] = sid
            }
        }
        for (text in texts) {
            if (cacheFile(text).exists()) continue
            t.synthesizeToFile(text, null, cacheFile(text), "cache:" + cacheKey(text))
        }
        trimCache(20 * 1024 * 1024)
    }

    private fun cacheKey(text: String): String {
        val key = MessageDigest.getInstance("SHA-256")
            .digest("$text|${tts?.defaultEngine}|${Locale.getDefault()}|$speechRate".toByteArray())
            .joinToString("") { "%02x".format(it) }
        return key
    }

    private fun cacheFile(text: String): File = File(cacheDir, "${cacheKey(text)}.wav")

    /** Cached pool playback; false = fall back to live TTS. */
    private fun playCached(text: String): Boolean {
        val sid = synchronized(cachedIds) { cachedIds[cacheKey(text)] } ?: return false
        soundPool?.play(sid, volume, volume, 1, 0, 1.0f)
        return true
    }

    private fun trimCache(maxBytes: Long) {
        val files = cacheDir.listFiles()?.sortedBy { it.lastModified() } ?: return
        var total = files.sumOf { it.length() }
        for (f in files) {
            if (total <= maxBytes) break
            total -= f.length()
            f.delete()
        }
    }

    /** Mute check: media volume 0 on connect + every 30 s (spec 6). */
    fun isMuted(): Boolean {
        val am = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        return am.getStreamVolume(AudioManager.STREAM_MUSIC) == 0
    }

    fun stop() {
        soundPool?.release()
        soundPool = null
        tts?.shutdown()
        tts = null
    }
}
