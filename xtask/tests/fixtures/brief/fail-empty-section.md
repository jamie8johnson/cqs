Branch: `feat/widget-knob`

# Lane feat/widget-knob: paint the widget's knob

MODEL: written by a person; this lane any model.

## Premises

1. The widget has three knobs, as its parts list counts them. [counted: `grep -c knob parts.txt`]
2. The knob's colour is set in one place, `colour.cfg` line 4. [opened: `colour.cfg:4`]
3. The paint dries in an hour. [unchecked: not timed]

## Task statements

Change the knob's colour:

```text
colour = blue
```

| Part | Colour |
| --- | --- |
| knob | blue |

## Decided judgment calls



## Stopping

Stop when the knob is blue.
