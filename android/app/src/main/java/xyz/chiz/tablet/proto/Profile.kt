package xyz.chiz.tablet.proto

import org.json.JSONObject

/** Button + profile model parsed from the PC `profile` message (spec 11). */
data class ButtonDef(
    val id: String,
    val label: String,
    val speak: String,
    val kind: String,
    val col: Int,
    val row: Int,
    val colspan: Int = 1,
    val rowspan: Int = 1,
)

data class StripProfile(
    val id: String,
    val name: String,
    val cols: Int,
    val rows: Int,
    val buttons: List<ButtonDef>,
) {
    fun button(id: String): ButtonDef? = buttons.firstOrNull { it.id == id }
}

/** Parse the `profile` field of a `profile` control message. */
fun parseStripProfile(obj: JSONObject): StripProfile {
    val grid = obj.getJSONObject("grid")
    val arr = obj.getJSONArray("buttons")
    return StripProfile(
        id = obj.getString("id"),
        name = obj.getString("name"),
        cols = grid.getInt("cols"),
        rows = grid.getInt("rows"),
        buttons = (0 until arr.length()).map { i ->
            val b = arr.getJSONObject(i)
            val label = b.getString("label")
            ButtonDef(
                id = b.getString("id"),
                label = label,
                speak = b.optString("speak", label),
                kind = b.getString("kind"),
                col = b.getInt("col"),
                row = b.getInt("row"),
                colspan = b.optInt("colspan", 1),
                rowspan = b.optInt("rowspan", 1),
            )
        },
    )
}

/** Validate a profile file object (mirrors core validate_profile). */
fun validateStripProfile(obj: JSONObject): List<String> {
    val errs = mutableListOf<String>()
    val idOk = Regex("^[a-z0-9_-]{1,32}$")
    if (!idOk.matches(obj.optString("id", ""))) errs.add("bad profile id")
    val name = obj.optString("name", "")
    if (name.isEmpty() || name.length > 24) errs.add("bad profile name")
    val grid = obj.optJSONObject("grid")
    val cols = grid?.optInt("cols", 0) ?: 0
    val rows = grid?.optInt("rows", 0) ?: 0
    if (cols !in 1..3 || rows !in 1..12) errs.add("bad grid")
    val seen = mutableSetOf<String>()
    val cells = mutableSetOf<Pair<Int, Int>>()
    val arr = obj.optJSONArray("buttons") ?: return errs + "missing buttons"
    for (i in 0 until arr.length()) {
        val b = arr.getJSONObject(i)
        val bid = b.optString("id", "")
        if (!idOk.matches(bid)) errs.add("bad button id $bid")
        if (!seen.add(bid)) errs.add("duplicate button id $bid")
        if (b.optString("label", "").length > 16) errs.add("label too long $bid")
        if (b.optString("speak", b.optString("label", "")).length > 24) errs.add("speak too long $bid")
        if (b.optString("kind", "") !in listOf("tap", "hold", "toggle")) errs.add("bad kind $bid")
        val c = b.optInt("col", -1)
        val r = b.optInt("row", -1)
        val cs = b.optInt("colspan", 1)
        val rs = b.optInt("rowspan", 1)
        if (c !in 0 until cols || r !in 0 until rows || cs < 1 || rs < 1 || c + cs > cols || r + rs > rows) {
            errs.add("button out of grid $bid")
            continue
        }
        for (cc in c until c + cs) for (rr in r until r + rs) {
            if (!cells.add(cc to rr)) errs.add("overlapping button $bid")
        }
    }
    return errs
}
