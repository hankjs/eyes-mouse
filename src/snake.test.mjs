import assert from 'node:assert/strict'
import test from 'node:test'
import { CHARACTER_INSET, HEAD_RADIUS, HOP_DURATION, HOP_LANDING, SnakeGame } from './snake.ts'
import { BEAD_OUTLINE, BeadTail } from './tail.ts'

function moveTo(game, x, y, frameTime = 1 / 60) {
  game.setTarget({ x, y })
  let frames = 0
  while (game.advance(frameTime)) {
    assert.ok(++frames < 300)
  }
}

test('Ingrid can start in any direction and stop or leave the field', () => {
  const game = new SnakeGame(1000, 600, () => 0)
  moveTo(game, 340, 300)
  assert.ok(Math.abs(game.head.x - 340) <= 1)
  const stationary = game.head
  assert.equal(game.advance(1 / 30), false)
  assert.equal(game.head, stationary)

  game.setTarget({ x: 100, y: 40 })
  assert.equal(game.advance(1 / 60), true)
  game.setTarget(null)
  const paused = game.head
  assert.equal(game.advance(1), false)
  assert.equal(game.head, paused)
  assert.equal(game.gait, 'idle')
  moveTo(game, -20, 250)
  assert.ok(game.head.x >= CHARACTER_INSET)
  assert.ok(game.head.x <= CHARACTER_INSET + 1)
})

test('gait thresholds use Euclidean distance and the single display width', () => {
  for (const [dx, dy, gait] of [[120, 160, 'crawl'], [121, 160, 'walk'], [240, 320, 'walk'], [241, 320, 'jump']]) {
    const game = new SnakeGame(1400, 1000, () => 0, 1000)
    game.setTarget({ x: game.head.x + dx, y: game.head.y + dy })
    game.advance(0)
    assert.equal(game.gait, gait)
  }
})

test('a long portrait diagonal stretches four hops and reaches the target on the last landing', () => {
  const game = new SnakeGame(420, 1500, () => 0)
  moveTo(game, 390, 1450)
  const target = { x: 38, y: 50 }
  game.setTarget(target)
  const landings = new Map()
  let frames = 0
  while (game.advance(0.05)) {
    assert.equal(game.gait, 'jump')
    if (game.gaitTime % HOP_DURATION >= HOP_LANDING) {
      const hop = Math.floor(game.gaitTime / HOP_DURATION)
      if (!landings.has(hop)) landings.set(hop, Math.hypot(target.x - game.head.x, target.y - game.head.y))
    }
    assert.ok(++frames < 300)
  }
  const distances = [...landings.values()]
  assert.equal(distances.length, 4)
  assert.ok(distances.every((distance, i) => i === 0 || distance < distances[i - 1]))
  assert.ok(distances.at(-1) <= 1)
  assert.equal(game.facing, -1)
  assert.equal(game.gait, 'idle')
  assert.equal(game.gaitTime, 0)
  assert.ok(Math.hypot(target.x - game.head.x, target.y - game.head.y) <= 1)
})

test('an airborne retarget preserves the current hop and lands within four hops', () => {
  const game = new SnakeGame(900, 1600, () => 0)
  moveTo(game, 38, 38)
  game.setTarget({ x: 450, y: 38 })
  for (let i = 0; i < 18; i++) game.advance(1 / 60)
  assert.equal(game.gait, 'jump')
  const time = game.gaitTime
  const target = { x: 862, y: 1562 }
  game.setTarget(target)
  assert.equal(game.advance(1 / 60), true)
  assert.equal(game.gait, 'jump')
  assert.ok(game.gaitTime > time)
  const landings = new Map()
  let frames = 0
  while (game.advance(1 / 60)) {
    assert.equal(game.gait, 'jump')
    if (game.gaitTime % HOP_DURATION >= HOP_LANDING) {
      const hop = Math.floor(game.gaitTime / HOP_DURATION)
      if (!landings.has(hop)) landings.set(hop, Math.hypot(target.x - game.head.x, target.y - game.head.y))
    }
    assert.ok(++frames < 150)
  }
  assert.equal(landings.size, 4)
  assert.ok([...landings.values()].at(-1) <= 1)
})

test('bead links lag a sharp turn, swing through, and then settle', () => {
  const tail = new BeadTail(640, 480)
  const anchor = { x: 320, y: 240, angle: Math.PI - 0.5, scale: 2 }
  assert.equal(tail.follow(anchor, 0), false)
  const angleBeforeTurn = tail.beads.map(({ angle }) => angle)
  const turn = { ...anchor, x: 310, angle: 0.5 }
  assert.equal(tail.follow(turn, 1 / 60), true)
  const [first, second] = tail.beads
  assert.ok(first.angle > turn.angle + 1, 'the first bead still points toward its previous side')
  assert.ok(angleBeforeTurn[0] - first.angle > angleBeforeTurn[1] - second.angle, 'the distal bead moves later')
  let frames = 0
  let overshot = false
  while (tail.follow(turn, 1 / 60)) {
    overshot ||= first.angle < turn.angle - 0.05
    assert.ok(++frames < 50)
  }
  assert.ok(overshot)
  assert.ok(Math.abs(first.angle - turn.angle) < 0.02)
  const still = tail.beads.map(({ x, y }) => ({ x, y }))
  assert.equal(tail.follow(turn, 1), false)
  assert.deepEqual(tail.beads.map(({ x, y }) => ({ x, y })), still)
  assert.equal(tail.follow(anchor, 1 / 60, true), false)
  assert.ok(Math.abs(first.angle - anchor.angle) < 1e-6)
})

test('an edge-constrained tail keeps its final layout when it stops settling', () => {
  const tail = new BeadTail(1000, 600)
  for (let i = 0; i < 6; i++) tail.add({ radius: 10, color: '#ff8a72', x: 0, y: 0 })
  const anchor = { x: 500, y: 500, angle: Math.atan2(0.55, -1), scale: 2 }
  tail.follow(anchor, 0)
  const turn = { ...anchor, angle: Math.atan2(0.55, 1) }
  for (let i = 0; i < 60; i++) {
    const before = tail.beads.map(({ x, y }) => ({ x, y }))
    if (tail.follow(turn, 1 / 60)) continue
    assert.ok(tail.beads.every((bead, index) => Math.hypot(bead.x - before[index].x, bead.y - before[index].y) < 2))
    return
  }
  assert.fail('tail did not settle')
})

test('large and newly resized tails keep each link in bounds through all corners', () => {
  const tail = new BeadTail(320, 240)
  for (let i = 0; i < 8; i++) tail.add({ radius: 14, color: '#f59ed4', x: 0, y: 0 })
  const check = (anchor) => {
    let joint = anchor
    for (const bead of tail.beads) {
      const margin = (bead.radius + BEAD_OUTLINE) * anchor.scale - 0.001
      assert.ok(bead.x >= margin && bead.x <= 320 - margin)
      assert.ok(bead.y >= margin && bead.y <= 240 - margin)
      assert.ok(Math.abs(Math.hypot(bead.x - joint.x, bead.y - joint.y) - bead.length * anchor.scale) < 1e-6)
      joint = bead
    }
  }
  for (const [x, y, angle] of [[8, 10, -2.5], [312, 10, -0.5], [312, 232, 0.5], [8, 232, 2.5]]) {
    const anchor = { x, y, angle, scale: 3 }
    for (let i = 0; i < 20; i++) {
      tail.follow(anchor, 1)
      check(anchor)
    }
  }
  const resized = { x: 8, y: 232, angle: 2.5, scale: 1 }
  assert.equal(tail.follow(resized, 1 / 60), false)
  assert.equal(tail.beads.length, 10)
  check(resized)
})

test('the logical head can cross and reverse across the tail and keep eating in delayed frames', () => {
  const game = new SnakeGame(1000, 600, () => 0)
  game.tail.follow({ x: 475, y: 300, angle: 0, scale: 2 }, 0, true)
  const [first, second] = game.tail.beads
  assert.ok(Math.abs(first.y - game.head.y) < 1)
  assert.ok(second.x > game.head.x)
  moveTo(game, 560, 300, 1)
  assert.ok(game.head.x > second.x)
  moveTo(game, 480, 300, 1)
  assert.ok(game.head.x < first.x)
  const { x, y } = game.food[0]
  moveTo(game, x, y, 1)
  assert.equal(game.score, 1)
  assert.equal(game.tail.beads.length, 3)
})

test('each food adds one bead of its color and an exhausted batch is replaced at open spots', () => {
  const firstBatch = [0, 0, 0.65, 0.5]
  const game = new SnakeGame(1000, 600, () => firstBatch.shift() ?? 0.999)
  assert.equal(game.food.length, 1)
  assert.equal(game.tail.beads.length, 2)
  const food = game.food[0]
  game.tail.follow({ x: 600, y: 300, angle: Math.PI, scale: 2 }, 0, true)
  moveTo(game, food.x, food.y)
  assert.equal(game.score, 1)
  assert.equal(game.tail.beads.length, 3)
  assert.equal(game.tail.beads.at(-1).color, food.color)
  assert.equal(game.food.length, 3)
  assert.equal(new Set(game.food.map(({ radius }) => radius)).size, 3)
  for (const pellet of game.food) {
    assert.ok(Math.hypot(pellet.x - game.head.x, pellet.y - game.head.y) > pellet.radius + HEAD_RADIUS)
    assert.equal(game.tail.overlaps(pellet, pellet.radius), false)
    for (const other of game.food) {
      if (other !== pellet) {
        assert.ok(Math.hypot(pellet.x - other.x, pellet.y - other.y) > pellet.radius + other.radius)
      }
    }
  }
  const next = game.food[0]
  moveTo(game, next.x, next.y)
  assert.equal(game.tail.beads.length, 2 + game.score)
  assert.equal(game.tail.beads.at(-1).color, next.color)
  assert.ok(game.tail.beads.at(-1).radius > game.tail.beads[2].radius)
  assert.equal(new SnakeGame(1000, 600, () => 0).tail.beads.length, 2)
})

test('food generation returns when the field is too small', () => {
  const tiny = new SnakeGame(20, 20, () => 0.999)
  assert.equal(tiny.food.length, 0)
})

test('food spawned at the edge stays reachable by the whole-character movement bounds', () => {
  const game = new SnakeGame(1000, 600, () => 0)
  assert.equal(game.food[0].x, CHARACTER_INSET)
  assert.equal(game.food[0].y, CHARACTER_INSET)
  moveTo(game, 0, 0)
  assert.equal(game.score, 1)
})
