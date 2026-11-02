import kotlinx.coroutines.*
fun row(key: String, value: Any?) = println("{\"case\":\"$key\",\"value\":\"$value\"}")
fun main() {
    val parent = Job(); val a = Job(parent); Job(parent).complete()
    val list = parent.children.iterator(); list.next(); val late = Job(parent); a.complete()
    row("list:promotion-survives-removal", list.hasNext() && list.next() === late)
    late.complete(); parent.complete()
    val callbacks = Job(); val first = Job(callbacks); callbacks.invokeOnCompletion { }
    val observed = callbacks.children.iterator(); observed.next(); val attached = Job(callbacks); first.complete()
    row("list:callback-promotes", observed.hasNext() && observed.next() === attached)
    attached.complete(); callbacks.complete()
}
