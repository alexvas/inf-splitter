## ADDED Requirements

### Requirement: Split System Instruction Send Planning

Interactions split-send for oversized system instructions SHALL build one ordered plan for all split pieces before sending upstream. Each planned piece SHALL define its input content, optional system instruction fragment, and whether tools and generation configuration are included. Streaming and non-streaming split-send paths SHALL consume the same plan order when constructing upstream chunk requests.

#### Scenario: Streaming and non-streaming use same piece order
- **WHEN** an interactions request has a system instruction that must be split and multiple content chunks remain
- **THEN** both streaming and non-streaming split-send construct pieces in this order: each system-instruction fragment first, the first content chunk attached to the final system-instruction piece when present, then remaining content chunks
- **AND** only the first planned piece includes tools and generation configuration

#### Scenario: Planned piece count drives in-flight batch
- **WHEN** a split system-instruction request creates an in-flight batch
- **THEN** the batch piece count matches the number of planned split pieces
- **AND** each sent piece uses its plan index for mark-started and acknowledgement updates
