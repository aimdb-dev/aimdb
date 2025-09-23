---
mode: agent
description: "Break down complex AimDB features into structured, actionable tasks with proper dependencies and platform considerations"
---

# AimDB Task Planning and Breakdown

## Task Definition

Create a structured task breakdown for implementing a specified AimDB feature with proper sequencing, dependencies, and platform considerations.

### Requirements
- **Feature Scope**: Define clear boundaries and deliverables for the feature
- **Task Granularity**: Break down into manageable tasks (4-8 hours each)
- **Platform Coverage**: Consider embedded/edge/cloud implementation differences
- **Dependencies**: Identify task dependencies and critical path
- **Risk Assessment**: Identify potential blockers and mitigation strategies

### Constraints
- Follow AimDB's async-first architecture principles
- Maintain compatibility across all target platforms (MCU/edge/cloud)
- Ensure tasks align with performance requirements (<50ms reactivity)
- Consider memory constraints for embedded targets
- Include testing and documentation tasks
- Respect existing API contracts and interfaces

### Success Criteria
- [ ] Feature broken down into implementable tasks (4-8 hours each)
- [ ] Clear task dependencies and sequencing identified
- [ ] Platform-specific considerations documented
- [ ] Risk assessment with mitigation strategies
- [ ] Acceptance criteria defined for each task
- [ ] Testing strategy integrated into task plan
- [ ] Documentation requirements specified
- [ ] Resource allocation and timeline estimates

## Planning Framework

### Task Categories

#### 1. Core Implementation Tasks
- **Engine/Database**: Core data processing and storage
- **Networking**: Protocol connectors and bridges
- **Adapters**: Runtime integration and platform abstraction
- **Performance**: Optimization and profiling
- **API**: Public interface definition and implementation

#### 2. Platform-Specific Tasks
- **Embedded (MCU)**: `no_std` implementation with Embassy
- **Edge**: Resource-constrained optimization
- **Cloud**: Scalability and distributed features

#### 3. Quality Assurance Tasks
- **Unit Testing**: Component-level test coverage
- **Integration Testing**: End-to-end workflow validation
- **Performance Testing**: Benchmark and profiling
- **Documentation**: API docs, guides, and examples

### Task Structure Template
```
Task ID: [COMPONENT]-[FEATURE]-[SEQUENCE]
Title: [Descriptive task title]
Category: [Core/Platform/QA]
Priority: [High/Medium/Low]
Effort: [Hours estimate]
Dependencies: [List of prerequisite tasks]
Platform: [All/Embedded/Edge/Cloud]

Description:
[Clear description of what needs to be implemented]

Acceptance Criteria:
- [ ] [Specific, testable outcome]
- [ ] [Performance requirement if applicable]
- [ ] [Platform compatibility verification]

Technical Notes:
- [Implementation approach]
- [Performance considerations]
- [Platform-specific requirements]

Risks:
- [Potential blockers or challenges]
- [Mitigation strategies]
```

## Implementation Guidelines

### Feature Analysis Process
1. **Requirements Gathering**
   - Define feature scope and boundaries
   - Identify user stories and use cases
   - Determine platform requirements
   - Assess performance implications

2. **Architecture Design**
   - Design component interactions
   - Define data flow and state management
   - Plan async operation patterns
   - Consider error handling strategies

3. **Task Decomposition**
   - Break down into atomic, testable units
   - Sequence tasks based on dependencies
   - Estimate effort and complexity
   - Identify critical path and milestones

4. **Risk Assessment**
   - Identify technical challenges
   - Assess platform compatibility risks
   - Plan mitigation strategies
   - Define fallback options

### Task Sequencing Principles

#### Phase 1: Foundation
- Core data structures and types
- Basic component scaffolding
- Interface definitions
- Configuration setup

#### Phase 2: Core Logic
- Main algorithm implementation
- State management
- Business logic
- Error handling

#### Phase 3: Integration
- Component integration
- Protocol implementation
- Platform adapters
- Performance optimization

#### Phase 4: Quality Assurance
- Comprehensive testing
- Performance validation
- Documentation
- Examples and demos

### Platform Considerations

#### Embedded/MCU Tasks
- `no_std` compatibility verification
- Memory usage optimization
- Interrupt safety validation
- Embassy runtime integration
- Hardware abstraction layer

#### Edge Device Tasks
- Resource constraint optimization
- Local storage implementation
- Network resilience features
- Power management considerations
- Field update mechanisms

#### Cloud Deployment Tasks
- Containerization setup
- Scalability testing
- Distributed coordination
- Monitoring integration
- Multi-tenant support

## Example Task Breakdown

### Feature: MQTT Protocol Bridge Implementation

#### Task 1: MQTT-001-FOUNDATION
**Title**: MQTT Protocol Bridge Core Structure
**Category**: Core  
**Priority**: High  
**Effort**: 6 hours  
**Dependencies**: None  
**Platform**: All  

**Description**: Implement the basic MQTT bridge structure with connection management and message routing.

**Acceptance Criteria**:
- [ ] MQTT client connection establishment
- [ ] Basic pub/sub message handling
- [ ] Connection state management
- [ ] Error handling for connection failures
- [ ] Configuration structure defined

**Technical Notes**:
- Use `rumqttc` for MQTT client implementation
- Implement async connection management
- Design for reconnection resilience

**Risks**:
- Network connectivity issues in embedded environments
- Mitigation: Implement robust retry logic with exponential backoff

#### Task 2: MQTT-002-INTEGRATION
**Title**: AimDB Message Integration
**Category**: Core  
**Priority**: High  
**Effort**: 8 hours  
**Dependencies**: MQTT-001-FOUNDATION  
**Platform**: All  

**Description**: Integrate MQTT bridge with AimDB's internal message system and data synchronization.

**Acceptance Criteria**:
- [ ] Bidirectional message translation (MQTT ↔ AimDB)
- [ ] Topic mapping configuration
- [ ] QoS level handling
- [ ] Message serialization/deserialization
- [ ] Throughput meets performance requirements (>1000 msg/s)

**Technical Notes**:
- Implement zero-copy message translation where possible
- Use efficient serialization format (MessagePack/CBOR)
- Design for low-latency processing (<10ms per message)

**Risks**:
- Serialization overhead impacting performance
- Mitigation: Use binary protocols and implement message pooling

### Risk Management

#### Common Risk Categories
1. **Technical Risks**
   - Platform compatibility issues
   - Performance bottlenecks
   - Third-party dependency problems
   - Memory constraints on embedded targets

2. **Integration Risks**
   - API breaking changes
   - Component interface mismatches
   - Async runtime conflicts
   - Protocol version incompatibilities

3. **Timeline Risks**
   - Task underestimation
   - Dependency delays
   - Scope creep
   - Testing complexity

#### Mitigation Strategies
- **Prototyping**: Build minimal viable implementations early
- **Platform Testing**: Validate on target hardware frequently
- **Incremental Integration**: Integrate components progressively
- **Performance Monitoring**: Continuous benchmarking during development

## Output Format

Provide the task breakdown in the following structure:
1. **Feature Overview**: Scope, objectives, and success metrics
2. **Task List**: Structured task breakdown with dependencies
3. **Timeline**: Estimated schedule with milestones
4. **Risk Assessment**: Identified risks and mitigation plans
5. **Resource Requirements**: Skills, tools, and infrastructure needed
6. **Testing Strategy**: Quality assurance approach
7. **Documentation Plan**: Required documentation deliverables