import subprocess

import matplotlib as mpl
from matplotlib.lines import Line2D
import matplotlib.pyplot as plt
import numpy as np

fig, axs = plt.subplots(1, 2, sharey=True, tight_layout=True)

success: dict[str, list[int]] = {}
failure: dict[str, list[int]] = {}
rsl_success: dict[str, list[int]] = {}
rsl_failure: dict[str, list[int]] = {}
for line in subprocess.run(
    ['cargo', 'run', '--release', '--', '--github-user=fenhl', '-xintercal', '--suite', 'bench', '--raw-data', '--uncompressed'],
    stdout=subprocess.PIPE, encoding='utf-8', check=True,
).stdout.splitlines():
    kind, count, worker = line.split(' ', 2)
    match kind:
        case 's':
            success.setdefault(worker, []).append(int(count))
        case 'f':
            failure.setdefault(worker, []).append(int(count))
        case 'S':
            rsl_success.setdefault(worker, []).append(int(count))
        case 'F':
            rsl_failure.setdefault(worker, []).append(int(count))

def draw_plot(success, failure, path):
    num_bins = int(np.ceil(np.sqrt(np.average([*(len(v) for v in success.values()), *(len(v) for v in failure.values())]))))
    bins = np.linspace(0, max(max(max(w) for w in success.values()), max(max(w) for w in failure.values())), num_bins)

    axs[0].hist(success.values(), bins, histtype='step', linewidth=2, alpha=0.7, label=[f'successes ({w})' for w in success])

    axs[1].hist(failure.values(), bins, histtype='step', linewidth=2, alpha=0.7, label=[f'failures ({w})' for w in success])
    axs[1].legend(loc='upper left')

    for ax in axs:
        # Edit legend to get lines as legend keys instead of the default polygons
        # and sort the legend entries in alphanumeric order
        handles, labels = ax.get_legend_handles_labels()
        leg_entries = {}
        for h, label in zip(handles, labels):
            leg_entries[label] = Line2D([0], [0], color=h.get_facecolor()[:-1],
                                        alpha=h.get_alpha(), lw=h.get_linewidth())
        labels, lines = zip(*reversed(leg_entries.items()))
        ax.legend(lines, labels, loc='upper left')

    fig.savefig(path)

draw_plot(success, failure, 'assets/worker-plot.svg')
if rsl_success or rsl_failure:
    draw_plot(rsl_success, rsl_failure, 'assets/worker-plot-rsl.svg')
