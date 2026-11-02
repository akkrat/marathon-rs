// Spike-only public-key authentication through the local ssh-agent. The remote-mac worker accepts only a passphrase-protected key
// held by the agent, and the Kotlin marathon's loadKeys takes no passphrase, so the harness signs through the agent instead.
// Everything after authentication is the Kotlin marathon's own code.
import net.schmizz.sshj.common.Buffer
import net.schmizz.sshj.common.KeyType
import net.schmizz.sshj.common.SSHPacket
import net.schmizz.sshj.userauth.keyprovider.KeyProvider
import net.schmizz.sshj.userauth.method.AuthPublickey
import java.net.UnixDomainSocketAddress
import java.nio.ByteBuffer
import java.nio.channels.SocketChannel
import java.security.PrivateKey
import java.security.PublicKey

object SshAgent {
    private const val REQUEST_IDENTITIES: Byte = 11
    private const val IDENTITIES_ANSWER: Byte = 12
    private const val SIGN_REQUEST: Byte = 13
    private const val SIGN_RESPONSE: Byte = 14

    private fun call(request: ByteArray): Buffer.PlainBuffer {
        val path = System.getenv("SSH_AUTH_SOCK") ?: error("SSH_AUTH_SOCK is not set")
        SocketChannel.open(UnixDomainSocketAddress.of(path)).use { channel ->
            val out = ByteBuffer.allocate(4 + request.size).putInt(request.size).put(request).flip()
            while (out.hasRemaining()) channel.write(out)
            val length = ByteBuffer.allocate(4)
            while (length.hasRemaining()) check(channel.read(length) >= 0) { "agent closed the connection" }
            val body = ByteBuffer.allocate(length.flip().int)
            while (body.hasRemaining()) check(channel.read(body) >= 0) { "agent closed the connection" }
            return Buffer.PlainBuffer(body.array())
        }
    }

    /** The first ssh-ed25519 identity of the agent */
    fun ed25519Identity(): ByteArray {
        val reply = call(byteArrayOf(REQUEST_IDENTITIES))
        check(reply.readByte() == IDENTITIES_ANSWER) { "unexpected agent reply" }
        repeat(reply.readUInt32AsInt()) {
            val blob = reply.readBytes()
            reply.readString()
            if (Buffer.PlainBuffer(blob).readString() == "ssh-ed25519") return blob
        }
        error("the agent holds no ssh-ed25519 key")
    }

    fun sign(blob: ByteArray, data: ByteArray): ByteArray {
        val request = Buffer.PlainBuffer().putByte(SIGN_REQUEST).putBytes(blob).putBytes(data).putUInt32(0).compactData
        val reply = call(request)
        check(reply.readByte() == SIGN_RESPONSE) { "the agent refused to sign" }
        return reply.readBytes()
    }
}

private class AgentKeyProvider(blob: ByteArray) : KeyProvider {
    private val publicKey = Buffer.PlainBuffer(blob).readPublicKey()
    override fun getPrivate(): PrivateKey = throw UnsupportedOperationException("the private key stays in the agent")
    override fun getPublic(): PublicKey = publicKey
    override fun getType(): KeyType = KeyType.fromKey(publicKey)
}

class AgentAuth(private val blob: ByteArray = SshAgent.ed25519Identity()) : AuthPublickey(AgentKeyProvider(blob)) {
    override fun putPubKey(reqBuf: SSHPacket): SSHPacket {
        reqBuf.putString("ssh-ed25519").putBytes(blob)
        return reqBuf
    }

    override fun putSig(reqBuf: SSHPacket): SSHPacket {
        val data = Buffer.PlainBuffer().putString(params.transport.sessionID).putBuffer(reqBuf).compactData
        reqBuf.putBytes(SshAgent.sign(blob, data))
        return reqBuf
    }
}
