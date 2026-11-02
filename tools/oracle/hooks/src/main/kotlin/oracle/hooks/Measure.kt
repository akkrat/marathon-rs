@file:OptIn(ExperimentalContracts::class)

package oracle.hooks

import kotlin.contracts.ExperimentalContracts
import kotlin.contracts.InvocationKind
import kotlin.contracts.contract

/**
 * `kotlin.system.measureTimeMillis` with both reads going through the wall clock of [label] ([OracleHooks.currentTimeMillis]).
 *
 * Top level, not a member of [OracleHooks], because only top-level functions may carry the `callsInPlace` contract that the
 * replaced call sites rely on (a `val` assigned inside the block). The name stays `measureTimeMillis` so that the
 * `return@measureTimeMillis` labels of the call sites keep resolving.
 */
inline fun measureTimeMillis(label: String, block: () -> Unit): Long {
    contract { callsInPlace(block, InvocationKind.EXACTLY_ONCE) }
    val start = OracleHooks.currentTimeMillis(label)
    block()
    return OracleHooks.currentTimeMillis(label) - start
}
