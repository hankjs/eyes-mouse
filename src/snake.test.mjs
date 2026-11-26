import assert from 'node:assert/strict'
import test from 'node:test'
import { HEAD_RADIUS, SnakeGame } from './snake.ts'

const controls = { left: 16, top: 16, right: 250, bottom: 90 }

function moveTo(game, x, y, frameTime = 1 / 60) {
  game.setTarget({ x, y })
  let frames = 0
  while (game.advance(frameTime)) {
    assert.ok(++frames < 300)
  }
}

test('the snake can start in any direction and stop or leave the field', () => {
  const game = new SnakeGame(1000, 600, controls, () => 0)
  moveTo(game, 340, 300)
  assert.equal(game.crashed, false)
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
  moveTo(game, -20, 250)
  assert.ok(game.head.x >= HEAD_RADIUS)
  assert.ok(game.head.x <= HEAD_RADIUS + 1)
})

test('curved motion leaves a continuous trail without a near-neck collision', () => {
  const game = new SnakeGame(1000, 600, controls, () => 0)
  for (let turn = 1; turn <= 12; turn++) {
    const angle = -Math.PI / 2 + turn * Math.PI / 24
    moveTo(game, 500 + 28 * Math.cos(angle), 328 + 28 * Math.sin(angle))
  }
  assert.equal(game.crashed, false)
  assert.ok(game.trail.length > 12)
  for (let i = 1; i < game.trail.length; i++) {
    assert.ok(game.trail[i].distance - game.trail[i - 1].distance <= 4.000001)
  }
})

test('the head hits the established body during a delayed frame', () => {
  const game = new SnakeGame(1000, 600, controls, () => 0)
  moveTo(game, 530, 300)
  moveTo(game, 530, 325)
  moveTo(game, 507, 325)
  assert.equal(game.crashed, false)
  moveTo(game, 507, 260, 1)
  assert.equal(game.crashed, true)
  assert.ok(game.head.y > 300)
})

test('eating grows the trail and replaces an exhausted batch at open spots', () => {
  const firstBatch = [0, 0, 0.65, 0.5]
  const game = new SnakeGame(1000, 600, controls, () => firstBatch.shift() ?? 0.999)
  assert.equal(game.food.length, 1)
  moveTo(game, 600, 300)
  const initialLength = game.head.distance - game.trail[0].distance
  moveTo(game, 670, 300)
  assert.equal(game.score, 1)
  assert.ok(game.head.distance - game.trail[0].distance > initialLength)
  assert.equal(game.food.length, 3)
  assert.equal(new Set(game.food.map(({ radius }) => radius)).size, 3)
  for (const pellet of game.food) {
    assert.ok(pellet.x > controls.right + pellet.radius || pellet.y > controls.bottom + pellet.radius)
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

test('food generation returns when the field is too small or fully covered', () => {
  const tiny = new SnakeGame(20, 20, controls, () => 0.999)
  assert.equal(tiny.food.length, 0)
  const covered = new SnakeGame(1000, 600, { left: 0, top: 0, right: 1000, bottom: 600 }, () => 0.999)
  assert.equal(covered.food.length, 0)
  moveTo(covered, 800, 300)
  assert.equal(covered.food.length, 0)
})
