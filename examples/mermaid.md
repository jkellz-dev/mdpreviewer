# Lorem Mermaid

Lorem ipsum dolor sit amet, one of each common mermaid diagram type. All dates
and values are fixed so the output is repeatable.

## Flowchart

```mermaid
flowchart LR
    A[Lorem] --> B{Ipsum?}
    B -- dolor --> C[Sit amet]
    B -- consectetur --> D[Adipiscing]
    C --> E((Elit))
    D --> E
```

## Sequence

```mermaid
sequenceDiagram
    participant L as Lorem
    participant I as Ipsum
    L->>I: dolor sit amet
    I-->>L: consectetur
    L->>L: adipiscing elit
    Note over L,I: sed do eiusmod tempor
```

## Class

```mermaid
classDiagram
    class Lorem {
        +String ipsum
        +dolor() int
    }
    class Sit {
        +amet() bool
    }
    Lorem <|-- Sit
```

## State

```mermaid
stateDiagram-v2
    [*] --> Lorem
    Lorem --> Ipsum: dolor
    Ipsum --> Sit: amet
    Sit --> [*]
```

## Entity Relationship

```mermaid
erDiagram
    LOREM ||--o{ IPSUM : dolor
    IPSUM }|--|{ SIT : amet
```

## Gantt

```mermaid
gantt
    title Lorem Ipsum Schedule
    dateFormat YYYY-MM-DD
    section Dolor
    Sit amet        :a1, 2026-01-05, 7d
    Consectetur     :after a1, 5d
    section Adipiscing
    Elit sed        :2026-01-12, 10d
```

## Pie

```mermaid
pie title Lorem Ipsum
    "Dolor" : 42
    "Sit" : 28
    "Amet" : 30
```

## Mindmap

```mermaid
mindmap
  root((Lorem))
    Ipsum
      Dolor
      Sit
    Amet
      Consectetur
```

## Git Graph

```mermaid
gitGraph
    commit id: "lorem"
    branch ipsum
    commit id: "dolor"
    checkout main
    commit id: "sit"
    merge ipsum
```

## Block

```mermaid
block
columns 3
  a["Lorem"] b["Ipsum"] c["Dolor"]
  space:3
  d(("Sit")) space e["Amet"]
  a --> d
  c --> e
```

## Packet

```mermaid
packet
title Lorem Packet
+16: "Lorem Port"
+16: "Ipsum Port"
32-47: "Dolor"
48-63: "Sit"
64-95: "Amet (variable length)"
```

## Sankey

```mermaid
sankey

Lorem,Ipsum,40
Lorem,Dolor,25
Sit,Ipsum,15
Ipsum,Amet,35
Ipsum,Consectetur,20
Dolor,Consectetur,25
```

## XY Chart

```mermaid
xychart
    title "Lorem Ipsum Revenue"
    x-axis [jan, feb, mar, apr, may, jun]
    y-axis "Dolor (in $)" 4000 --> 11000
    bar [5000, 6000, 7500, 8200, 9500, 10500]
    line [5000, 6000, 7500, 8200, 9500, 10500]
```

## Radar

```mermaid
radar-beta
  title Lorem Ipsum Scores
  axis l["Lorem"], i["Ipsum"], d["Dolor"]
  axis s["Sit"], a["Amet"], c["Consectetur"]
  curve x["Adipiscing"]{85, 90, 80, 70, 75, 90}
  curve y["Elit"]{70, 75, 85, 80, 90, 85}

  max 100
  min 0
```

## Treemap

```mermaid
treemap-beta
"Lorem"
    "Ipsum": 10
    "Dolor": 20
"Sit"
    "Amet": 15
    "Consectetur": 25
```

## Venn

```mermaid
venn-beta
  title "Lorem overlap"
  set Lorem
  set Ipsum
  union Lorem,Ipsum["Dolor"]
```

## Ishikawa

```mermaid
ishikawa-beta
    Lorem Ipsum
    Dolor
        Sit amet
        Consectetur
    Adipiscing
        Elit sed
    Eiusmod
        TEMPOR
            Incididunt
            Ut labore
    Magna
        Aliqua
```

## Wardley

```mermaid
wardley-beta
title Lorem Value Chain

anchor Lorem [0.95, 0.63]
component Ipsum [0.79, 0.61]
component Dolor [0.63, 0.81]
component Sit [0.52, 0.80]
component Amet [0.43, 0.35]

Lorem -> Ipsum
Ipsum -> Dolor
Ipsum -> Sit
Sit -> Amet

evolve Amet 0.62

note "Consectetur adipiscing elit" [0.30, 0.49]
```

## Cynefin

```mermaid
cynefin-beta
  title Lorem Ipsum

  complex
    "Dolor sit amet"

  complicated
    "Consectetur adipiscing"

  clear
    "Sed do eiusmod"

  chaotic
    "Tempor incididunt"

  confusion
    "Ut labore"
```

## Swimlane

```mermaid
swimlane-beta LR
  subgraph Lorem
    a[Ipsum dolor]
    e[Sit amet]
  end

  subgraph Consectetur
    b[Adipiscing]
    c[Elit sed]
  end

  a --> b
  b -->|eiusmod| c
  c --> e
```

## Event Modeling

```mermaid
eventmodeling

tf 01 ui LoremUI
tf 02 cmd AddIpsum { dolor: string }
tf 03 evt IpsumAdded { dolor: string }
tf 04 rmo IpsumList
```

## Tree View

```mermaid
treeView-beta
├── lorem/
│   ├── ipsum.rs
│   └── dolor.rs
├── Cargo.toml
└── README.md
```

## Railroad

```mermaid
railroad-beta
title Lorem Grammar

lorem = sequence(
    nonterminal("ipsum"),
    zeroOrMore(sequence(
        choice(terminal("+"), terminal("-")),
        nonterminal("ipsum")
    ))
) ;
```

```mermaid
railroad-ebnf-beta
title "Lorem Digit"

digit = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;
```

```mermaid
railroad-abnf-beta
title "Lorem Address"

address = local-part "@" domain ;
local-part = 1*( ALPHA / DIGIT / "." / "-" ) ;
domain = label *( "." label ) ;
label = 1*( ALPHA / DIGIT / "-" ) ;
```

```mermaid
railroad-peg-beta
title "Lorem Calculator"

Expression <- Term (("+" / "-") Term)* ;
Term <- Number (("*" / "/") Number)* ;
Number <- Digit+ ;
Digit <- "0" / "1" / "2" / "3" / "4" / "5" / "6" / "7" / "8" / "9" ;
```

## Invalid Diagram

The block below is intentionally broken, to show how render errors look.

```mermaid
flowchart LR
    A[Lorem --> B
```

Neque porro quisquam est, qui dolorem ipsum quia dolor sit amet.
