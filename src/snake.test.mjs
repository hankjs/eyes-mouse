import assert from 'node:assert/strict'
import test from 'node:test'
import { CHARACTER_INSET, FOOD_LIFETIME_MS, HEAD_RADIUS, HOP_DURATION, HOP_LANDING, SnakeGame } from './snake.ts'
import { LIGHT_TIERS, LightOrbit } from './lights.ts'

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

test('ground chases on a wide display leave time for pointer sampling and drawing', () => {
  const displayWidth = 5120
  const motionBudget = 1.75
  for (const fps of [30, 60]) {
    for (const [distance, expectedGaits] of [[displayWidth / 5, ['crawl']], [displayWidth * 2 / 5, ['walk', 'crawl']]]) {
      const game = new SnakeGame(5000, 2160, () => 0, displayWidth)
      const target = { x: game.head.x + distance, y: game.head.y }
      game.setTarget(target)
      const gaits = new Set()
      for (let frame = 0; frame < Math.floor(fps * motionBudget); frame++) {
        if (!game.advance(1 / fps)) break
        gaits.add(game.gait)
      }
      assert.equal(game.gait, 'idle', `${distance}px at ${fps}Hz should finish within ${motionBudget}s`)
      assert.ok(Math.hypot(target.x - game.head.x, target.y - game.head.y) <= 1)
      assert.deepEqual([...gaits], expectedGaits)
    }
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

const stopped = { moving: false, reduced: false }
const moving = { moving: true, reduced: false }
const anchor = { x: 300, y: 300, scale: 2, radiusX: 60, radiusY: 38 }

function pellet(tier, x = 400, y = 350) {
  return { x, y, tier, radius: LIGHT_TIERS[tier].radius }
}

function tiers(orbit) {
  return orbit.lights.map(({ tier }) => tier)
}

test('three matching lights merge into the next tier, and three large lights leave a finite seal', () => {
  const orbit = new LightOrbit(1400, 800)
  assert.equal(orbit.follow(anchor, 0, stopped), true)
  assert.deepEqual(tiers(orbit), [0, 0])
  const small = pellet(0)
  orbit.add(small)
  assert.deepEqual(tiers(orbit), [1])
  const [merge] = orbit.effects
  assert.equal(merge.kind, 'merge')
  assert.equal(merge.tier, 1)
  assert.deepEqual(merge.sources.at(-1), { tier: 0, x: small.x, y: small.y })
  assert.equal(orbit.lights[0].opacity, 0)
  orbit.follow(anchor, 0.25, stopped)
  assert.ok(merge.progress > 0 && merge.progress < 1)
  assert.ok(orbit.lights[0].opacity > 0 && orbit.lights[0].opacity < 1)
  orbit.follow(anchor, 0.15, stopped)
  assert.equal(orbit.lights[0].opacity, 1)
  assert.equal(orbit.effects.length, 0)

  orbit.add(pellet(1))
  assert.deepEqual(tiers(orbit), [1, 1])
  orbit.add(pellet(1))
  assert.deepEqual(tiers(orbit), [2])
  assert.equal(orbit.effects[0].tier, 2)
  assert.deepEqual(orbit.effects[0].sources.map(({ tier }) => tier), [1, 1, 1])
  orbit.follow(anchor, 0.4, stopped)

  orbit.add(pellet(2))
  assert.deepEqual(tiers(orbit), [2, 2])
  orbit.add(pellet(2))
  assert.deepEqual(tiers(orbit), [])
  assert.deepEqual(orbit.effects.map(({ kind }) => kind), ['seal'])
  assert.deepEqual(orbit.effects[0].sources.map(({ tier }) => tier), [2, 2, 2])
  assert.equal(orbit.follow(anchor, 0.5, stopped), true)
  assert.equal(orbit.follow(anchor, 0.6, stopped), false)
  assert.deepEqual(orbit.effects, [])
})

test('one pickup can trigger both merges and then the seal in sequence', () => {
  const orbit = new LightOrbit(1400, 800)
  orbit.follow(anchor, 0, stopped)
  for (const tier of [1, 2, 1, 2]) orbit.add(pellet(tier))
  assert.equal(orbit.effects.length, 0)
  orbit.add(pellet(0))
  assert.deepEqual(orbit.lights, [])
  assert.deepEqual(orbit.effects.map(({ kind, tier }) => [kind, tier]), [['merge', 1], ['merge', 2], ['seal', 2]])
  const active = () => orbit.effects.filter(({ progress }) => progress > 0).map(({ kind, tier }) => [kind, tier])
  orbit.follow(anchor, 0.2, stopped)
  assert.deepEqual(active(), [['merge', 1]])
  orbit.follow(anchor, 0.2, stopped)
  assert.deepEqual(active(), [['merge', 2]])
  orbit.follow(anchor, 0.36, stopped)
  assert.deepEqual(active(), [['seal', 2]])
  assert.equal(orbit.follow(anchor, 1.1, stopped), false)
})

test('a later pickup waits for earlier merges and keeps existing lights visible until they gather', () => {
  const orbit = new LightOrbit(1400, 800)
  orbit.follow(anchor, 0, stopped)
  for (const tier of [1, 1, 2, 0]) orbit.add(pellet(tier))
  orbit.follow(anchor, 0.05, stopped)
  orbit.add(pellet(2))
  const active = () => orbit.effects.filter(({ progress }) => progress > 0).map(({ kind, tier }) => [kind, tier])
  const visible = () => orbit.waitingLights.filter(({ opacity }) => opacity > 0).map(({ tier }) => tier)
  assert.deepEqual(active(), [['merge', 1]])
  assert.deepEqual(visible(), [1, 1, 2, 2])
  orbit.follow(anchor, 0.33, stopped)
  assert.deepEqual(active(), [['merge', 2]])
  assert.deepEqual(visible(), [2, 2])
  orbit.follow(anchor, 0.33, stopped)
  assert.deepEqual(active(), [['merge', 2]])
  assert.deepEqual(visible(), [2, 2, 2])
  orbit.follow(anchor, 0.03, stopped)
  assert.deepEqual(active(), [['seal', 2]])
  assert.deepEqual(orbit.waitingLights, [])
})

test('rapid pickups keep the orbit and transient effects bounded', () => {
  const orbit = new LightOrbit(1400, 800)
  orbit.follow(anchor, 0, stopped)
  const totals = [2, 0, 0]
  let seals = 0
  for (let index = 0; index < 1000; index++) {
    const tier = index < 500 ? 0 : index % 3
    orbit.add(pellet(tier))
    totals[tier]++
    for (let type = 0; type < totals.length; type++) {
      if (totals[type] < 3) continue
      totals[type] -= 3
      if (type < 2) totals[type + 1]++
      else seals++
    }
    assert.ok(orbit.effects.length <= 3)
    assert.deepEqual([0, 1, 2].map((type) => orbit.lights.filter((light) => light.tier === type).length), totals)
  }
  assert.ok(seals > 0)
})

function pairedOrbits(width, height, start) {
  const orbits = [new LightOrbit(width, height), new LightOrbit(width, height)]
  for (const orbit of orbits) {
    orbit.add(pellet(1))
    orbit.add(pellet(2))
    orbit.follow(start, 0, stopped)
  }
  return orbits
}

function distances(orbit, reference) {
  return orbit.lights.map(({ x, y }, index) => Math.hypot(x - reference.lights[index].x, y - reference.lights[index].y))
}

test('small lights catch first, heavier lights lag, and trails retract while the stopped orbit keeps turning', () => {
  const [orbit, reference] = pairedOrbits(3000, 1000, anchor)
  for (let frame = 1; frame <= 60; frame++) {
    const next = { ...anchor, x: anchor.x + frame * 5 }
    orbit.follow(next, 1 / 60, moving)
    reference.follow(next, 1 / 60, { ...stopped, snap: true })
  }
  const atRest = { ...anchor, x: anchor.x + 300 }
  const lag = distances(orbit, reference)
  assert.ok(lag[0] > 10 && lag[2] > lag[0] && lag[3] > lag[2])
  assert.ok(orbit.lights.every(({ trailX, trailY }) => trailX < -10 && Math.abs(trailY) < 1e-6))
  for (let frame = 0; frame < 20; frame++) {
    const previous = orbit.lights.map(({ trailX }) => Math.abs(trailX))
    orbit.follow(atRest, 1 / 60, stopped)
    reference.follow(atRest, 1 / 60, { ...stopped, snap: true })
    assert.ok(orbit.lights.every(({ trailX }, index) => Math.abs(trailX) <= previous[index] + 1e-6))
  }
  assert.ok(distances(orbit, reference).every((lag) => lag < 1))
  const trails = orbit.lights.map(({ trailX }) => Math.abs(trailX))
  assert.ok(trails.every((length) => length < 1))
  orbit.follow(atRest, 1.5, stopped)
  reference.follow(atRest, 1.5, { ...stopped, snap: true })
  assert.ok(distances(orbit, reference).every((lag) => lag < 1e-6))
  assert.ok(orbit.lights.every(({ trailX, trailY }) => trailX === 0 && trailY === 0))
  const previous = orbit.lights.map(({ x, y }) => ({ x, y }))
  assert.equal(orbit.follow(atRest, 0.15, stopped), true)
  assert.ok(previous.every((point, index) => Math.hypot(orbit.lights[index].x - point.x, orbit.lights[index].y - point.y) > 2))
})

test('every light joins the same orbit within two seconds after far, irregular retargets', () => {
  const corners = [[38, 38], [38, 2122], [5082, 38], [5082, 2122]]
  const positions = corners.map(([x, y]) => ({ ...anchor, x, y }))
  const [orbit, reference] = pairedOrbits(5120, 2160, positions[0])
  const jitter = [1 / 30, 1 / 60, 1 / 120, 0.075, 0.012]
  const parked = { ...stopped, snap: true }
  for (const point of [...positions.slice(1), positions[0], positions[3]]) {
    orbit.follow(point, 1 / 30, stopped)
    reference.follow(point, 1 / 30, parked)
  }
  assert.ok(Math.min(...distances(orbit, reference)) > 100)
  let elapsed = 0
  for (let frame = 0; elapsed < 1.9; frame++) {
    const seconds = jitter[frame % jitter.length]
    elapsed += seconds
    orbit.follow(positions[3], seconds, stopped)
    reference.follow(positions[3], seconds, parked)
  }
  assert.ok(elapsed <= 2)
  assert.ok(distances(orbit, reference).every((lag) => lag < 1e-6))
  assert.ok(orbit.lights.every(({ trailX, trailY }) => trailX === 0 && trailY === 0))
})

test('orbits and trails stay on screen near corners even at the largest size', () => {
  const width = 320
  const height = 240
  const scale = 3
  const [orbit] = pairedOrbits(width, height, { ...anchor, scale })
  for (const [x, y] of [[8, 10], [312, 10], [312, 230], [8, 230]]) {
    for (let frame = 0; frame < 70; frame++) {
      orbit.follow({ x, y, scale, radiusX: 96, radiusY: 65 }, 1 / 30, frame < 10 ? moving : stopped)
      for (const light of orbit.lights) {
        const halo = 1.7 * LIGHT_TIERS[light.tier].orbitRadius * scale
        assert.ok(light.x >= halo && light.x <= width - halo)
        assert.ok(light.y >= halo && light.y <= height - halo)
        assert.ok(light.x + light.trailX >= 0 && light.x + light.trailX <= width)
        assert.ok(light.y + light.trailY >= 0 && light.y + light.trailY <= height)
      }
    }
  }
})

test('reduced motion follows instantly, freezes the orbit, and shows a finite static seal', () => {
  const orbit = new LightOrbit(400, 280)
  const reduced = { moving: false, reduced: true }
  assert.equal(orbit.follow(anchor, 0, reduced), false)
  const next = { ...anchor, x: 200, y: 140 }
  assert.equal(orbit.follow(next, 0, { ...reduced, moving: true }), false)
  const parked = orbit.lights.map(({ x, y }) => ({ x, y }))
  assert.equal(orbit.follow(next, 1, reduced), false)
  assert.deepEqual(orbit.lights.map(({ x, y }) => ({ x, y })), parked)
  orbit.add(pellet(0))
  assert.deepEqual(tiers(orbit), [1])
  assert.equal(orbit.lights[0].opacity, 1)
  assert.equal(orbit.effects.length, 0)
  for (let count = 0; count < 3; count++) orbit.add(pellet(2))
  assert.equal(orbit.follow(next, 0.4, reduced), true)
  const [seal] = orbit.effects
  assert.equal(seal.kind, 'seal')
  assert.equal(orbit.follow({ ...next, x: 30, y: 30 }, 0.4, reduced), true)
  assert.deepEqual({ x: seal.x, y: seal.y }, { x: next.x, y: next.y })
  assert.equal(orbit.follow(next, 0.4, reduced), false)
})

test('a delayed frame collects a pellet swept between its start and end positions', () => {
  let now = 0
  const game = new SnakeGame(10000, 1000, () => 0, 10000, () => now)
  const start = game.head.x
  const expired = { ...pellet(2, start + 30, game.head.y), expiresAt: FOOD_LIFETIME_MS }
  const food = { ...pellet(1, start + 100, game.head.y), expiresAt: FOOD_LIFETIME_MS + 500 }
  game.food.splice(0, game.food.length, expired, food)
  game.setTarget({ x: start + 3200, y: game.head.y })
  now = FOOD_LIFETIME_MS
  assert.equal(game.advance(1), true)
  assert.ok(game.head.x > food.x + HEAD_RADIUS + food.radius)
  assert.equal(game.score, 1)
  assert.deepEqual(tiers(game.orbit), [0, 0, 1])
  assert.deepEqual({ x: game.orbit.lights[2].x, y: game.orbit.lights[2].y }, { x: food.x, y: food.y })
})

test('picking up a light spawns replacements even when another remains', () => {
  const values = [0.12, 0, 0.65, 0.5, 0, 0.15, 0.3]
  const game = new SnakeGame(1000, 600, () => values.shift() ?? 0.999)
  assert.equal(game.food.length, 2)
  assert.deepEqual(tiers(game.orbit), [0, 0])
  const [food, survivor] = game.food
  game.orbit.follow({ ...anchor, x: 600 }, 0, stopped)
  moveTo(game, food.x, food.y)
  assert.equal(game.score, 1)
  assert.deepEqual(tiers(game.orbit), [1])
  assert.equal(game.food.length, 4)
  assert.equal(game.food[0], survivor)
  const replacements = game.food.slice(1)
  assert.equal(new Set(replacements.map(({ tier }) => tier)).size, 3)
  for (const light of replacements) {
    assert.equal(light.radius, LIGHT_TIERS[light.tier].radius)
    assert.ok(Math.hypot(light.x - game.head.x, light.y - game.head.y) > light.radius + HEAD_RADIUS)
    assert.equal(game.orbit.overlaps(light, light.radius), false)
    for (const other of game.food) {
      if (other !== light) assert.ok(Math.hypot(light.x - other.x, light.y - other.y) > light.radius + other.radius)
    }
  }
})

test('each light expires at its own deadline while the cat is still', () => {
  const values = [0.12, 0, 0.65, 0.5, 0, 0.15, 0.3]
  let now = 0
  const game = new SnakeGame(1000, 600, () => values.shift() ?? 0, 1000, () => now)
  const [food, survivor] = game.food
  now = 2000
  values.push(0.25, 0, 0.88, 0.85)
  moveTo(game, food.x, food.y)
  const replacement = game.food[1]
  assert.equal(replacement.expiresAt, now + FOOD_LIFETIME_MS)
  assert.equal(game.nextFoodUpdate, FOOD_LIFETIME_MS)

  game.setTarget(null)
  now = FOOD_LIFETIME_MS - 1
  game.updateFood()
  assert.deepEqual(game.food, [survivor, replacement])
  now++
  game.updateFood()
  assert.deepEqual(game.food, [replacement])
  assert.equal(game.nextFoodUpdate, replacement.expiresAt)
  now = replacement.expiresAt
  game.updateFood()
  assert.equal(game.food.length, 1)
  assert.equal(game.food.includes(replacement), false)
  assert.equal(game.score, 1)
  assert.deepEqual(tiers(game.orbit), [1])
})

test('simultaneous expirations roll separately and share the nine-light limit', () => {
  const values = [0.25]
  let now = 0
  const game = new SnakeGame(1000, 600, () => values.shift() ?? 0.999, 1000, () => now)
  assert.equal(game.food.length, 3)
  const initial = game.food.slice()
  values.push(0, 0.25, 0.75)
  now = FOOD_LIFETIME_MS
  game.updateFood()
  assert.equal(game.food.length, 4)
  assert.ok(initial.every((food) => !game.food.includes(food)))
  now += FOOD_LIFETIME_MS
  game.updateFood()
  assert.equal(game.food.length, 9)
  const capped = game.food.slice()
  game.updateFood()
  assert.deepEqual(game.food, capped)
  assert.equal(game.score, 0)
  assert.deepEqual(tiers(game.orbit), [0, 0])
})

test('the empty-field refill can reach nine even when replacement rolls are zero', () => {
  const values = [0]
  let now = 0
  const game = new SnakeGame(1000, 600, () => values.shift() ?? 0.999, 1000, () => now)
  assert.equal(game.food.length, 1)
  const [food] = game.food
  values.push(0, 0.999)
  moveTo(game, food.x, food.y)
  assert.equal(game.score, 1)
  assert.equal(game.food.length, 9)
})

test('food placement retries after the cat moves away from previously blocked spots', () => {
  let now = 0
  const game = new SnakeGame(130, 80, () => 0, 130, () => now)
  assert.equal(game.food.length, 0)
  moveTo(game, 92, 40)
  now = game.nextFoodUpdate - 1
  game.updateFood()
  assert.equal(game.food.length, 0)
  now++
  game.updateFood()
  assert.equal(game.food.length, 1)
  assert.equal(game.score, 0)
  moveTo(game, 38, 40)
  assert.equal(game.score, 1)
})
