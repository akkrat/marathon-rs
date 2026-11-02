// Copied unchanged from vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple/device/ConnectionFailures.kt at
// e47352f2: the installed distribution predates the file, so the harness compiles it itself.
package com.malinskiy.marathon.apple.device

import com.malinskiy.marathon.apple.logparser.parser.DeviceFailureException
import com.malinskiy.marathon.apple.logparser.parser.DeviceFailureReason
import net.schmizz.sshj.common.DisconnectReason
import net.schmizz.sshj.transport.TransportException
import java.net.SocketException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import java.util.concurrent.TimeoutException

private val TRANSIENT_DISCONNECTS = setOf(
    DisconnectReason.UNKNOWN,
    DisconnectReason.CONNECTION_LOST,
    DisconnectReason.TOO_MANY_CONNECTIONS,
    DisconnectReason.BY_APPLICATION,
    DisconnectReason.SERVICE_NOT_AVAILABLE,
    DisconnectReason.MAC_ERROR,
)

/**
 * Whether a connection to a worker that failed with [e] (see [ConnectionFactory.connectWorker]) can succeed later without a
 * configuration change: the worker's address could not be resolved or failed the reachability check, the network failed (no
 * route to host, connection refused or reset, a timeout, also of the key exchange or authentication) or the worker dropped the connection, e.g. sshd's MaxStartups. An
 * authentication failure, an unverifiable host key, a failed key exchange or an unreadable key are not transient
 */
fun isTransientConnectionFailure(e: Throwable): Boolean {
    if (e !is DeviceFailureException || e.reason != DeviceFailureReason.UnreachableHost) return false
    val cause = e.cause ?: return true
    return generateSequence(cause) { it.cause?.takeIf { next -> next !== it } }.take(MAX_CAUSES).any {
        when (it) {
            is TransportException -> it.disconnectReason in TRANSIENT_DISCONNECTS
            // sshj reports a reply that didn't arrive in time, e.g. to an authentication request, as a TimeoutException
            is SocketException, is SocketTimeoutException, is UnknownHostException, is TimeoutException -> true
            else -> false
        }
    }
}

private const val MAX_CAUSES = 16
