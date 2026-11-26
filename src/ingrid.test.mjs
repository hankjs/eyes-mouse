import assert from 'node:assert/strict'
import test from 'node:test'
import { ChaseAnimation, IDLE_GESTURES, idlePose, pickAnimation } from './ingrid.ts'

test('one chase keeps its animation through retargets, delayed frames and brief rests', () => {
  const rolls = [0, 0.1, 0.99, 0.99]
  const chase = new ChaseAnimation(() => {
    assert.ok(rolls.length, 'animation only rolls at the start of a new chase')
    return rolls.shift()
  })

  chase.advance(0, 0.02, true, false)
  const startingRun = chase.pose(1, 'crawl', false)
  assert.equal(startingRun.sprite, 'walkRight')
  chase.advance(250, 0.05, true, false)
  assert.equal(chase.pose(-1, 'jump', false).sprite, 'walkLeft')
  chase.advance(270, 0.02, false, false)
  assert.equal(chase.pose(1, 'idle', false).sprite, 'idle')
  chase.advance(350, 0.02, true, false)
  const resumedRun = chase.pose(1, 'walk', false)
  assert.equal(resumedRun.sprite, 'walkRight')
  assert.notEqual(resumedRun.lift, startingRun.lift)

  chase.advance(500, 0.02, false, false)
  chase.advance(560, 0.02, true, false)
  assert.equal(chase.pose(-1, 'jump', false).sprite, 'crawlReachLeft')
  chase.advance(760, 0.02, false, false)
  chase.advance(800, 0.02, true, false)
  assert.equal(chase.pose(-1, 'crawl', false).sprite, 'jumpAirLeft')
  chase.stop()
  chase.advance(820, 0.02, true, false)
  assert.equal(chase.pose(-1, 'walk', false).sprite, 'crawlReachLeft')
  assert.equal(rolls.length, 0)
})

test('flight rises, stays aloft and lands after the controller stops moving', () => {
  const chase = new ChaseAnimation(() => 0.99)
  const flight = []
  for (let frame = 1; frame <= 45; frame++) {
    assert.equal(chase.advance(frame * 20, 0.02, true, false), true)
    flight.push(chase.pose(1, 'jump', false).lift)
  }
  assert.ok(flight[0] > 0 && flight[0] < 3)
  assert.ok(flight.slice(7).every((lift) => lift >= 17 && lift <= 23))
  assert.equal(chase.pose(-1, 'jump', false).sprite, 'jumpAirLeft')

  assert.equal(chase.advance(920, 0.02, false, false), true)
  assert.ok(chase.pose(1, 'idle', false).lift > 10)
  for (let frame = 2; frame <= 8; frame++) chase.advance(900 + frame * 20, 0.02, false, false)
  assert.ok(chase.pose(1, 'idle', false).lift < 2)
  assert.equal(chase.advance(1100, 0.04, false, false), false)
  assert.deepEqual(chase.pose(1, 'idle', false), { sprite: 'idle' })
})

test('reduced motion clears flight and preserves stationary directional poses for each controller gait', () => {
  const chase = new ChaseAnimation(() => 0.99)
  chase.advance(50, 0.05, true, false)
  assert.ok(chase.pose(1, 'crawl', false).lift > 0)
  assert.deepEqual(chase.pose(1, 'crawl', true), { sprite: 'crawlRight' })
  assert.deepEqual(chase.pose(-1, 'jump', true), { sprite: 'walkLeft' })
  assert.equal(chase.advance(60, 0.01, false, true), false)
  assert.deepEqual(chase.pose(-1, 'idle', true), { sprite: 'idle' })
  assert.deepEqual(chase.pose(-1, 'idle', false), { sprite: 'idle' })
})

test('idle gestures vary and their decorations disappear before the gesture ends', () => {
  const first = pickAnimation(IDLE_GESTURES, undefined, () => 0)
  const second = pickAnimation(IDLE_GESTURES, first, () => 0)
  assert.equal(first, 'heart')
  assert.equal(second, 'snack')
  assert.equal(pickAnimation(IDLE_GESTURES, second, () => 0.99), 'peek')
  for (const gesture of IDLE_GESTURES) {
    assert.equal(idlePose(gesture, 1, 0).accent, undefined)
    assert.equal(idlePose(gesture, 1, 1).accent, undefined)
  }

  const heart = idlePose('heart', 1, 0.35).accent
  assert.equal(heart.kind, 'heart')
  assert.equal(heart.opacity, 1)
  assert.ok(idlePose('heart', 1, 0.65).accent.y < heart.y)
  assert.equal(idlePose('heart', 1, 0.96).accent, undefined)

  for (const facing of [-1, 1]) {
    const snack = idlePose('snack', facing, 0.18).accent
    const bite = idlePose('snack', facing, 0.45).accent
    assert.equal(snack.kind, 'snack')
    assert.ok(snack.x * facing > bite.x * facing && snack.radius > bite.radius)
    assert.equal(idlePose('snack', facing, 0.55).accent, undefined)
    for (const chewing of [1 / 6, 1 / 2, 5 / 6]) {
      assert.ok(idlePose('snack', facing, 0.44 + 0.51 * chewing).scaleY < 0.96)
    }
  }
  assert.equal(idlePose('peek', 1, 0.5).sprite, 'peek')
})
