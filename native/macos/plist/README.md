# Property lists

Host initialization property lists are converted to the strict `PropertyDictionary` contract in
`nwipc-renderer-bootstrap`. Missing, mistyped, unknown, and mismatched fields fail before memory or
signal providers attach. `xtask` owns generated bundle compatibility metadata.
