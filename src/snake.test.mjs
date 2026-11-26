import assert from 'node:assert/strict'
import test from 'node:test'
import { CHARACTER_INSET, HEAD_RADIUS, SnakeGame } from './snake.ts'

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

test('gaits use Euclidean distance with hysteresis and return to idle after arrival', () => {
  const game = new SnakeGame(1000, 600, () => 0)
  game.setTarget({ x: 535, y: 335 })
  game.advance(0)
  assert.equal(game.gait, 'crawl')
  game.setTarget({ x: 542, y: 342 })
  game.advance(0)
  assert.equal(game.gait, 'crawl')
  game.setTarget({ x: 548, y: 348 })
  game.advance(0)
  assert.equal(game.gait, 'walk')
  game.setTarget({ x: 542, y: 342 })
  game.advance(0)
  assert.equal(game.gait, 'walk')

  const target = { x: 340, y: 140 }
  game.setTarget(target)
  const gaits = new Set()
  let frames = 0
  while (game.advance(1 / 60)) {
    gaits.add(game.gait)
    assert.ok(++frames < 300)
  }
  assert.deepEqual([...gaits], ['jump', 'walk', 'crawl'])
  assert.equal(game.facing, -1)
  assert.equal(game.gait, 'idle')
  assert.equal(game.gaitTime, 0)
  assert.ok(Math.hypot(target.x - game.head.x, target.y - game.head.y) <= 1)
})

test('curved motion leaves a continuous trail', () => {
  const game = new SnakeGame(1000, 600, () => 0)
  for (let turn = 1; turn <= 12; turn++) {
    const angle = -Math.PI / 2 + turn * Math.PI / 24
    moveTo(game, 500 + 28 * Math.cos(angle), 328 + 28 * Math.sin(angle))
  }
  assert.ok(game.trail.length > 12)
  for (let i = 1; i < game.trail.length; i++) {
    assert.ok(game.trail[i].distance - game.trail[i - 1].distance <= 4.000001)
  }
})

test('the head can cross the body in a delayed frame, reverse along it and keep eating', () => {
  const game = new SnakeGame(1000, 600, () => 0)
  moveTo(game, 530, 300)
  moveTo(game, 530, 325)
  moveTo(game, 507, 325)
  game.setTarget({ x: 507, y: 230 })
  assert.equal(game.advance(1), true)
  assert.equal(game.advance(1), true)
  assert.ok(game.head.y < 300)
  assert.ok(game.trail.some((point) => point.y === 300 && point.x > game.head.x))
  moveTo(game, 507, 260, 1)
  assert.ok(Math.abs(game.head.y - 260) <= 1)
  moveTo(game, 507, 290)
  assert.ok(Math.abs(game.head.y - 290) <= 1)
  assert.ok(game.trail.some((point) => point.distance < game.head.distance - 34
    && Math.hypot(point.x - game.head.x, point.y - game.head.y) < 4))
  moveTo(game, 507, 325)
  assert.ok(Math.abs(game.head.y - 325) <= 1)
  const initialLength = game.head.distance - game.trail[0].distance
  const { x, y } = game.food[0]
  moveTo(game, x, y)
  assert.equal(game.score, 1)
  assert.ok(game.head.distance - game.trail[0].distance > initialLength)
})

test('eating grows the trail and replaces an exhausted batch at open spots', () => {
  const firstBatch = [0, 0, 0.65, 0.5]
  const game = new SnakeGame(1000, 600, () => firstBatch.shift() ?? 0.999)
  assert.equal(game.food.length, 1)
  moveTo(game, 600, 300)
  const initialLength = game.head.distance - game.trail[0].distance
  moveTo(game, 670, 300)
  assert.equal(game.score, 1)
  assert.ok(game.head.distance - game.trail[0].distance > initialLength)
  assert.equal(game.food.length, 3)
  assert.equal(new Set(game.food.map(({ radius }) => radius)).size, 3)
  for (const pellet of game.food) {
    for (const point of game.trail) {
      assert.ok(Math.hypot(pellet.x - point.x, pellet.y - point.y) > pellet.radius + HEAD_RADIUS)
    }
    for (const other of game.food) {
      if (other !== pellet) {
        assert.ok(Math.hypot(pellet.x - other.x, pellet.y - other.y) > pellet.radius + other.radius)
      }
    }
  }
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
