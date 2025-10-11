# Buffer Architecture Diagrams

## Current vs New Architecture

### Current Architecture (Sequential, Blocking)

```
┌──────────────────────────────────────────────────────────┐
│                    User Code                             │
│                 db.produce(value)                        │
└────────────────────────┬─────────────────────────────────┘
                         │
                         ▼
┌──────────────────────────────────────────────────────────┐
│              TypedRecord::produce()                      │
└────────────────────────┬─────────────────────────────────┘
                         │
                         ▼
             ┌───────────────────────┐
             │   Producer Function   │
             │  (validation/xform)   │
             └───────────┬───────────┘
                         │ await
                         ▼
             ┌───────────────────────┐
             │   Consumer 1 Function │
             │   (database logger)   │
             └───────────┬───────────┘
                         │ await
                         ▼
             ┌───────────────────────┐
             │   Consumer 2 Function │
             │   (metrics sender)    │
             └───────────┬───────────┘
                         │ await
                         ▼
             ┌───────────────────────┐
             │   Consumer 3 Function │
             │   (alert checker)     │
             └───────────────────────┘

Problem: Producer blocks on slowest consumer
```

### New Architecture (Async, Parallel)

```
┌──────────────────────────────────────────────────────────┐
│                    User Code                             │
│                 db.produce(value)                        │
└────────────────────────┬─────────────────────────────────┘
                         │
                         ▼
┌──────────────────────────────────────────────────────────┐
│              TypedRecord::produce()                      │
└────────────────────────┬─────────────────────────────────┘
                         │
                         ▼
             ┌───────────────────────┐
             │   Producer Function   │
             │  (validation/xform)   │
             └───────────┬───────────┘
                         │ await
                         ▼
             ┌───────────────────────┐
             │   buffer.push(value)  │◄── Non-blocking!
             │      (enqueue)        │
             └───────────────────────┘
                         │
          ┌──────────────┼──────────────┐
          │              │              │
          ▼              ▼              ▼
    ┌─────────┐    ┌─────────┐    ┌─────────┐
    │ Reader1 │    │ Reader2 │    │ Reader3 │
    └────┬────┘    └────┬────┘    └────┬────┘
         │              │              │
         │              │              │
    Dispatcher     Dispatcher     Dispatcher
    (async task)   (async task)   (async task)
         │              │              │
    ┌────▼────┐    ┌────▼────┐    ┌────▼────┐
    │recv()   │    │recv()   │    │recv()   │
    │  .await │    │  .await │    │  .await │
    └────┬────┘    └────┬────┘    └────┬────┘
         │              │              │
    ┌────▼────┐    ┌────▼────┐    ┌────▼────┐
    │Consumer1│    │Consumer2│    │Consumer3│
    │Function │    │Function │    │Function │
    └─────────┘    └─────────┘    └─────────┘

Benefits: Parallel execution, non-blocking producer
```

## Buffer Type Internals

### SPMC Ring Buffer (tokio::sync::broadcast)

```
Producer                                    Consumers
   │                                           │
   │ push(value)                              │
   ▼                                           │
┌─────────────────────────────────────┐       │
│        Circular Buffer              │       │
│  ┌───┬───┬───┬───┬───┬───┬───┬───┐ │       │
│  │ 1 │ 2 │ 3 │ 4 │ 5 │ 6 │ 7 │ 8 │ │       │
│  └───┴───┴───┴───┴───┴───┴───┴───┘ │       │
│         ▲           ▲       ▲        │       │
│         │           │       │        │       │
│      Reader1    Reader2  Reader3    │       │
│      pos=2      pos=5    pos=7      │       │
└─────────────────────────────────────┘       │
                                              │
   ┌──────────────────────────────────────────┘
   │
   ▼ subscribe()
┌──────────────┐
│ Reader Handle│
│ (independent │
│  position)   │
└──────────────┘

Overflow Behavior:
  - New value overwrites oldest
  - Lagging readers get RecvErr::Lagged(n)
  - Fast readers unaffected
```

### SingleLatest (tokio::sync::watch)

```
Producer                                    Consumers
   │                                           │
   │ push(new_value)                          │
   ▼                                           │
┌─────────────────────────┐                   │
│   Single Slot Storage   │                   │
│  ┌──────────────────┐   │                   │
│  │  Option<T>       │   │                   │
│  │  Some(latest)    │◄──┼───────────────────┤
│  └──────────────────┘   │                   │
│         ▲                │                   │
│         │ overwrites     │                   │
│         │ previous       │                   │
└─────────────────────────┘                   │
                                              │
   ┌──────────────────────────────────────────┘
   │
   ▼ subscribe()
┌──────────────┐
│Watcher Handle│
│ (notified on │
│   changes)   │
└──────────────┘

Behavior:
  - Only latest value stored
  - All readers see same value
  - Intermediate values skipped
  - No lag possible
```

### Mailbox (Mutex + Notify)

```
Producer                                    Consumer
   │                                           │
   │ push(value)                              │
   ▼                                           │
┌─────────────────────────────────────────────┤
│            Mailbox                          │
│  ┌──────────────────────────────────────┐  │
│  │  Mutex<Option<T>>                    │  │
│  │  ┌────────────┐                      │  │
│  │  │ Some(val)  │◄─ overwrite if full  │  │
│  │  └────────────┘                      │  │
│  └──────────────────────────────────────┘  │
│             │                               │
│             │                               │
│  ┌──────────▼──────────────────────────┐   │
│  │  Notify                              │   │
│  │  (wakes waiting consumer)            │   │
│  └──────────────────────────────────────┘  │
└─────────────────────────────────────────────┤
                                              │
                                              ▼
                                       ┌──────────────┐
                                       │ recv().await │
                                       │ (take value) │
                                       └──────────────┘

Behavior:
  - Single slot (one value at a time)
  - New value overwrites old
  - Notify wakes consumer
  - Guaranteed delivery of ≥1 value
```

## Dispatcher Lifecycle

```
                    AimDbBuilder::build()
                            │
                            ▼
                  ┌─────────────────────┐
                  │ Validate all records│
                  └──────────┬──────────┘
                            │
                            ▼
            ┌───────────────────────────────┐
            │ for each record:              │
            │   record.spawn_dispatchers()  │
            └───────────────┬───────────────┘
                            │
          ┌─────────────────┼─────────────────┐
          │                 │                 │
          ▼                 ▼                 ▼
    ┌─────────┐       ┌─────────┐       ┌─────────┐
    │Dispatch1│       │Dispatch2│       │Dispatch3│
    │  Task   │       │  Task   │       │  Task   │
    └────┬────┘       └────┬────┘       └────┬────┘
         │                 │                 │
         │                 │                 │
    ┌────▼──────────────────▼─────────────────▼────┐
    │          Async Runtime Executor               │
    │  (tokio::spawn or embassy::spawn)            │
    └───────────────────────────────────────────────┘
         │                 │                 │
         │  Loop:          │  Loop:          │  Loop:
         │    recv()       │    recv()       │    recv()
         │    ▼            │    ▼            │    ▼
         │  consumer()     │  consumer()     │  consumer()
         │    ▼            │    ▼            │    ▼
         │  handle lag     │  handle lag     │  handle lag
         │    │            │    │            │    │
         └────┘            └────┘            └────┘

Dispatcher Loop:
┌─────────────────────────────────────┐
│ loop {                              │
│   match reader.recv().await {       │
│     Ok(item) =>                     │
│       consumer.call(em, item).await │
│     Err(Lagged(n)) =>               │
│       log_warn("lagged by {}", n)   │
│     Err(Closed) =>                  │
│       break  // graceful exit       │
│   }                                 │
│ }                                   │
└─────────────────────────────────────┘
```

## Cross-Record Emission Flow

```
User produces to Record A:
  │
  ▼
┌──────────────────────┐
│  Record A Producer   │
│                      │
│  emitter.emit::<B>(x)│◄── Cross-emit to Record B
└──────────┬───────────┘
           │
           ▼
    Buffer A.push()
           │
           └──────────────────┐
                             │
           ┌─────────────────┘
           ▼
    Dispatcher A
           │
           ▼
┌──────────────────────┐
│  Record A Consumer   │
│                      │
│  emitter.emit::<C>(y)│◄── Cross-emit to Record C
└──────────┬───────────┘
           │
           ▼
    Buffer C.push()
           │
           ▼
    Dispatcher C
           │
           ▼
┌──────────────────────┐
│  Record C Consumer   │
│  (process)           │
└──────────────────────┘

Benefits:
  - No circular dependencies (async dispatch)
  - No deadlocks (non-blocking push)
  - Clean separation of concerns
```

## Memory Layout Comparison

### SPMC Ring (1024 capacity, 100 byte items)

```
┌─────────────────────────────────────────────┐
│          Buffer Memory                      │
│  1024 × 100 bytes = 100 KB                  │
│  ┌───────────────────────────────────────┐  │
│  │ [item][item][item]...[item][item]     │  │
│  └───────────────────────────────────────┘  │
└─────────────────────────────────────────────┘

Per-Consumer Overhead (3 consumers):
┌────────┬────────┬────────┐
│Reader1 │Reader2 │Reader3 │  ~64 bytes each
│ pos: 0 │ pos: 5 │ pos: 10│  = 192 bytes
└────────┴────────┴────────┘

Total: ~100 KB + 192 bytes
```

### SingleLatest (100 byte items)

```
┌─────────────────────┐
│   Single Slot       │
│  Option<T>          │
│  100 bytes          │
│  ┌───────────────┐  │
│  │  Some(value)  │  │
│  └───────────────┘  │
└─────────────────────┘

Per-Consumer Overhead (3 consumers):
┌────────┬────────┬────────┐
│Watch1  │Watch2  │Watch3  │  ~16 bytes each
└────────┴────────┴────────┘  = 48 bytes

Total: ~100 bytes + 48 bytes
```

### Mailbox (100 byte items)

```
┌─────────────────────┐
│   Mailbox Slot      │
│  Mutex<Option<T>>   │
│  100 bytes + mutex  │
│  ┌───────────────┐  │
│  │  Some(value)  │  │
│  └───────────────┘  │
└──────────┬──────────┘
           │
┌──────────▼──────────┐
│   Notify Handle     │
│   ~32 bytes         │
└─────────────────────┘

Per-Consumer Overhead (3 consumers):
┌────────┬────────┬────────┐
│Notify1 │Notify2 │Notify3 │  ~16 bytes each
└────────┴────────┴────────┘  = 48 bytes

Total: ~132 bytes + 48 bytes
```

---

**Legend:**
- `│` `─` `┌` `└` `┐` `┘` : Box drawing
- `▼` `▲` : Data flow direction
- `◄──` : Reference/pointer
- `...` : Continuation/omission
