package xyz.chiz.tablet.net

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo

/** mDNS browsing for `_chiz._tcp` + manual address entry (spec 3). */
@Suppress("DEPRECATION") // resolveService works on minSdk 29..34; the API-34 callback is optional.
class Discovery(context: Context, private val onFound: (name: String, host: String, port: Int) -> Unit) {
    private val nsd = context.getSystemService(Context.NSD_SERVICE) as NsdManager
    private var discovering = false

    private val discoveryListener = object : NsdManager.DiscoveryListener {
        override fun onDiscoveryStarted(regType: String) {}
        override fun onServiceFound(service: NsdServiceInfo) {
            if (service.serviceType != "_chiz._tcp.") return
            try {
                nsd.resolveService(service, resolveListener)
            } catch (e: Exception) {
            }
        }
        override fun onServiceLost(service: NsdServiceInfo) {}
        override fun onDiscoveryStopped(serviceType: String) {}
        override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
            stop()
        }
        override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {}
    }

    private val resolveListener = object : NsdManager.ResolveListener {
        override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {}
        override fun onServiceResolved(serviceInfo: NsdServiceInfo) {
            onFound(serviceInfo.serviceName, serviceInfo.host.hostAddress ?: return, serviceInfo.port)
        }
    }

    fun start() {
        if (discovering) return
        discovering = true
        try {
            nsd.discoverServices("_chiz._tcp.", NsdManager.PROTOCOL_DNS_SD, discoveryListener)
        } catch (e: Exception) {
            discovering = false
        }
    }

    fun stop() {
        if (!discovering) return
        discovering = false
        try {
            nsd.stopServiceDiscovery(discoveryListener)
        } catch (e: Exception) {
        }
    }
}
