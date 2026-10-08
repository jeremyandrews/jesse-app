// Diet tracking data — rewritten on each food/exercise log
// Dashboard-Fancy.html loads this via <script src="diet-today.js">
// Reload the HTML page in browser to pick up changes
window.DIET_TODAY = {
  date: "2026-10-04",
  dayStyle: "normal",
  dayType: "",
  weight: { lbs: 186, kg: 84.4, bf: 25.1, mm: 138.8, notes: "Morning weigh-in. BF via Health." },
  exercise: [
    { type: "Run", time: "07:00", desc: "Easy run, outdoor", distance: 8.02, duration: "0:46:10", calories: 560, treadmill: false, src: "watch" }
  ],
  meals: [
    { name: "Breakfast", time: "08:10", items: [
      { item: "Banana, medium, raw", amount: "1 medium (~118g edible)", cal: 105, p: 1.3, f: 0.4, c: 27, fiber: 3.1, na: 1, satf: 0.1, sug: 14.4, k: 422, ca: 6, o3: null, mg: 32, chol: 0, tfat: 0, asug: 0, pur: 57, hg: 0, se: 1.2, vd: 0, caf: 0, iod: null, fe: 0.3, ret: null, ox: null, o6: 0.1, alc: 0, cat: "food", src: "home", basis: "reference", tsrc: "actual" }
    ]},
    { name: "Lunch", time: "13:05", items: [
      { item: "Pasta with tomato sauce", amount: "~90g dry pasta, ~150g sauce", cal: 420, p: 13, f: 6, c: 79, fiber: 6.5, na: 520, satf: 0.9, sug: 9, k: 560, ca: 40, o3: null, mg: 60, chol: 0, tfat: 0, asug: 0, pur: null, hg: 0, se: 55, vd: 0, caf: 0, iod: null, fe: 2.6, ret: null, ox: null, o6: 1.2, alc: 0, cat: "food", src: "home", basis: "weighed", tsrc: "actual" }
    ]}
  ],
  targets: null,
  rolling7: {
    days: 7, from: "2026-09-28", to: "2026-10-04",
    nutrients: {
      mercury_ug: { known: 0, knownCount: 2, unknownCount: 0 },
      omega3_mg: { known: 0, knownCount: 0, unknownCount: 2 }
    }
  }
};
