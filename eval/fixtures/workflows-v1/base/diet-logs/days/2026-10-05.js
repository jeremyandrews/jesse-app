// Diet tracking data — rewritten on each food/exercise log
// Dashboard-Fancy.html loads this via <script src="diet-today.js">
// Reload the HTML page in browser to pick up changes
window.DIET_TODAY = {
  date: "2026-10-05",
  dayStyle: "normal",
  dayType: "",
  weight: { lbs: 185.6, kg: 84.2, bf: 25, mm: 138.8, notes: "Morning weigh-in. BF via Health." },
  exercise: [
    { type: "Walk", time: "18:30", desc: "Evening walk", distance: 4.1, duration: "0:48:00", calories: 190, treadmill: false, src: "self_reported" }
  ],
  meals: [
    { name: "Breakfast", time: "07:45", items: [
      { item: "Greek yogurt, plain, 2% fat", amount: "170g pot", cal: 124, p: 17, f: 3.3, c: 6.6, fiber: 0, na: 60, satf: 2.1, sug: 6.6, k: 240, ca: 190, o3: null, mg: 19, chol: 13, tfat: 0, asug: 0, pur: null, hg: 0, se: 16, vd: 0, caf: 0, iod: null, fe: 0.1, ret: null, ox: null, o6: 0.1, alc: 0, cat: "food", src: "home", basis: "label", tsrc: "actual" }
    ]},
    { name: "Dinner", time: "19:30", items: [
      { item: "Grilled chicken breast", amount: "~150g cooked", cal: 248, p: 46.5, f: 5.4, c: 0, fiber: 0, na: 111, satf: 1.5, sug: 0, k: 384, ca: 23, o3: null, mg: 44, chol: 128, tfat: 0, asug: 0, pur: 225, hg: 0, se: 41, vd: 0.2, caf: 0, iod: null, fe: 1.6, ret: null, ox: null, o6: 1.1, alc: 0, cat: "food", src: "home", basis: "estimate", tsrc: "actual" }
    ]}
  ],
  targets: null,
  rolling7: {
    days: 7, from: "2026-09-29", to: "2026-10-05",
    nutrients: {
      mercury_ug: { known: 0, knownCount: 4, unknownCount: 0 },
      omega3_mg: { known: 0, knownCount: 0, unknownCount: 4 }
    }
  }
};
